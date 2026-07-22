// =============================================================================
// Plik: codec.js
// Opis: Fasada nad wasm-bindgen glue dla tentaflow-protocol-wasm.
//       Abstrahuje init().then() w pojedyncze `codecReady` Promise i
//       eksportuje typed helpery do budowy binarych frameow WebSocket.
// Przyklad:
//   import { codecReady, encode } from '/js/protocol/codec.js';
//   await codecReady;
//   const frame = encode.meshNodeListRequest(nextCorrelationId());
//   ws.send(frame);
// =============================================================================

// Import glue generated przez wasm-bindgen (wyjscie `wasm-pack build --target web`).
// Plik `wasm_glue.js` + `wasm_glue_bg.wasm` sa produkowane przez build.rs (#32)
// i kopiowane do tego katalogu na release build.
import initWasm, * as wasm from './wasm_glue.js';

let _wasm = null;
let _messageKind = null;

/**
 * Promise rozwiazujacy sie gdy WASM codec jest zainicjalizowany.
 * WSZYSTKIE inne funkcje z tego modulu wymagaja uprzedniego `await codecReady`.
 */
export const codecReady = (async () => {
  await initWasm();
  _wasm = wasm;
  _messageKind = wasm.messageKind();
  return wasm;
})();

/**
 * Wersja schematu protokolu. Klient musi wyslac ten numer w MetaSchemaVersionCheck
 * przy handshake — mismatch z serwerem = disconnect.
 */
export function schemaVersion() {
  assertReady();
  return _wasm.SCHEMA_VERSION();
}

/**
 * Stale discriminantow message_kind (patrz tentaflow_protocol::envelope::message_kind).
 */
export function messageKind() {
  assertReady();
  return _messageKind;
}

// =============================================================================
// Encode helpery (build binary frames)
// =============================================================================

/**
 * Typed factory dla frameow do wyslania.
 *
 * Zwracaja Uint8Array gotowy do `ws.send(bytes)`.
 */
export const encode = {
  /** MessageBody::ModelListRequest — publiczny katalog modeli (Anonymous). */
  modelListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MetaSchemaVersionCheck — pierwszy frame po WSS upgrade. */
  metaSchemaVersionCheck(correlationId, clientVersion, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMetaSchemaVersionCheck(clientVersion);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_SCHEMA_VERSION_CHECK,
      body,
    );
  },

  /** MessageBody::MetaHeartbeat — keepalive (liczy RTT na RTT). */
  metaHeartbeat(correlationId, sentAtEpoch, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMetaHeartbeat(BigInt(sentAtEpoch));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MetaCancelStream — anulacja aktywnego streama po correlation_id. */
  metaCancelStream(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMetaCancelStream();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_CANCEL_STREAM,
      body,
    );
  },

  /** MessageBody::ApiKeyListRequest (unit). */
  apiKeyListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeApiKeyListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ApiKeyCreateRequest { name, keyType, subjectId?, scopeResources: {resourceType,resourceId}[] } */
  apiKeyCreateRequest(correlationId, { name, keyType = 'user', subjectId = null, scopeResources = [] }, sequence = 1) {
    assertReady();
    const types = scopeResources.map((r) => r.resourceType);
    const ids = scopeResources.map((r) => r.resourceId);
    const body = _wasm.encodeApiKeyCreateRequest(name, keyType, subjectId ?? undefined, types, ids);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ApiKeyScopeListRequest { keyUid } */
  apiKeyScopeListRequest(correlationId, { keyUid }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeApiKeyScopeListRequest(keyUid);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ApiKeyScopeSetRequest { keyUid, resourceType, resourceId, accessLevel } */
  apiKeyScopeSetRequest(correlationId, { keyUid, resourceType, resourceId, accessLevel }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeApiKeyScopeSetRequest(keyUid, resourceType, resourceId, accessLevel);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ApiKeyScopeClearRequest { keyUid, resourceType, resourceId } */
  apiKeyScopeClearRequest(correlationId, { keyUid, resourceType, resourceId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeApiKeyScopeClearRequest(keyUid, resourceType, resourceId);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ApiKeyRotateRequest { keyUid } */
  apiKeyRotateRequest(correlationId, { keyUid }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeApiKeyRotateRequest(keyUid);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /**
   * MessageBody::TranslateRequest { sourceText, sourceLang, targetLang, tone? }
   * Zwraca pojedynczy TranslateResponse (nie stream).
   */
  translateRequest(correlationId, { sourceText, sourceLang, targetLang, tone = null }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTranslateRequest(sourceText, sourceLang, targetLang, tone ?? undefined);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ApiKeyRevokeRequest { key_id } */
  apiKeyRevokeRequest(correlationId, { keyId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeApiKeyRevokeRequest(keyId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AuthLoginRequest { username, password } */
  authLoginRequest(correlationId, { username, password }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAuthLoginRequest(username, password);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AuthMeRequest (unit). */
  authMeRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAuthMeRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MePreferencesGetRequest (unit). */
  mePreferencesGetRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMePreferencesGetRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MePreferencesUpdateRequest { language }. */
  mePreferencesUpdateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMePreferencesUpdateRequest(payload?.language ?? null);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ChatStreamRequest (simplified: 1 user message). */
  chatStreamRequest(correlationId, { modelId, userMessage, flowId, sessionId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeChatStreamRequestSimple(modelId, userMessage, flowId ?? null, sessionId ?? null);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::FlowInvokeRequest — uniwersalny most do flow engine
   * (audio-only wariant dla chat audio). `audio` to Uint8Array WAV.
   */
  flowInvokeRequest(
    correlationId,
    { flowId, model, serviceType, mime, sampleRate, audio, language, sessionId },
    sequence = 1,
  ) {
    assertReady();
    const body = _wasm.encodeFlowInvokeAudio(
      flowId != null ? String(flowId) : undefined,
      model || '',
      serviceType || 'chat',
      mime || 'audio/wav',
      sampleRate ?? undefined,
      audio,
      language ?? undefined,
      sessionId ?? undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MeshNodeListRequest (unit). */
  meshNodeListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshNodeListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MeshNodeDetailRequest { nodeId } */
  meshNodeDetailRequest(correlationId, { nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshNodeDetailRequest(nodeId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MeshPendingListRequest (unit). */
  meshPendingListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshPendingListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MeshIdentityRequest (unit). */
  meshIdentityRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshIdentityRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MeshServicesListRequest (unit). */
  meshServicesListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshServicesListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MeshTrustedListRequest (unit). */
  meshTrustedListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshTrustedListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // ---- Mesh write ops (FAZA 1b) ----

  /** MeshPairingStartRequest { remoteAddress, remoteAddresses, remoteRelayUrl } */
  meshPairingStartRequest(correlationId, {
    remoteAddress,
    pin,
    pinHint,
    remotePublicKey,
    remoteAddresses,
    remoteRelayUrl,
    remoteHostname,
  }, sequence = 1) {
    assertReady();
    const hint = pinHint || pin || '';
    const body = _wasm.encodeMeshPairingStartRequest(
      remoteAddress,
      hint,
      remotePublicKey || '',
      Array.isArray(remoteAddresses) ? remoteAddresses : [],
      remoteRelayUrl || '',
      remoteHostname || '',
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MeshPairingConfirmRequest { pairId, pin } */
  meshPairingConfirmRequest(correlationId, { pairId, pin }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshPairingConfirmRequest(pairId, pin);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MeshPairingRejectRequest { pairId } */
  meshPairingRejectRequest(correlationId, { pairId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshPairingRejectRequest(pairId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MeshTrustRevokeRequest { nodeId } */
  meshTrustRevokeRequest(correlationId, { nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshTrustRevokeRequest(nodeId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MeshTrustRetrustRequest { nodeId } */
  meshTrustRetrustRequest(correlationId, { nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshTrustRetrustRequest(nodeId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // ---- Sync baseline-adopt admin (FAZA C krok 3) ----

  /** MessageBody::BaselineDonorListRequest (unit) — kandydaci na dawce. */
  baselineDonorListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBaselineDonorListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::BaselineAdoptStartRequest { donorNodeId } */
  baselineAdoptStartRequest(correlationId, { donorNodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBaselineAdoptStartRequest(String(donorNodeId || ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::BaselineAdoptStatusRequest (unit) — faza + raport. */
  baselineAdoptStatusRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBaselineAdoptStatusRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::BaselineAdoptClearRequest (unit) — odblokuj zawieszony stan. */
  baselineAdoptClearRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBaselineAdoptClearRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MeshConnectRequest { address } */
  meshConnectRequest(correlationId, { address }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshConnectRequest(address);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MeshNodeCommandRequest { nodeId, command, args } */
  meshNodeCommandRequest(correlationId, { nodeId, command, args = [] }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshNodeCommandRequest(nodeId, command, args);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MeshNodeNetworkConfigRequest { nodeId, interfaceName, configJson } */
  meshNodeNetworkConfigRequest(correlationId, { nodeId, interfaceName, configJson }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshNodeNetworkConfigRequest(nodeId, interfaceName, configJson);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterListRequest (unit). */
  clusterListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterDetailRequest { clusterId } */
  clusterDetailRequest(correlationId, { clusterId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterDetailRequest(clusterId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterCreateRequest. */
  clusterCreateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterCreateRequest(
      payload.name,
      payload.description ?? null,
      payload.strategy ?? 'distributed',
      !!payload.failoverEnabled,
      payload.failoverTarget ?? null,
      (payload.healthCheckIntervalMs ?? 5000) >>> 0,
      (payload.timeoutMs ?? 10000) >>> 0,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterDeleteRequest { clusterId } */
  clusterDeleteRequest(correlationId, { clusterId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterDeleteRequest(clusterId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterAddMemberRequest. */
  clusterAddMemberRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterAddMemberRequest(
      payload.clusterId,
      payload.nodeId,
      payload.interfaceType ?? null,
      payload.interfaceSpeedMbps ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterRemoveMemberRequest { clusterId, nodeId } */
  clusterRemoveMemberRequest(correlationId, { clusterId, nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterRemoveMemberRequest(clusterId, nodeId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterProbeStreamRequest { nodeIds: string[] } */
  clusterProbeStreamRequest(correlationId, { nodeIds }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterProbeStreamRequest(nodeIds);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterRdmaConfigureRequest { clusterId, sudoPassword, mtu? } */
  clusterRdmaConfigureRequest(correlationId, { clusterId, sudoPassword, mtu }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterRdmaConfigureRequest(
      clusterId,
      sudoPassword,
      mtu ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::ClusterDeployRequest — deploy one model split across the whole
   * cluster (vLLM tensor-parallel). Optional fields fall back to backend defaults.
   */
  clusterDeployRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterDeployRequest(
      payload.clusterId,
      payload.engineId,
      payload.modelRepo ?? null,
      payload.modelPresetId ?? null,
      payload.servedModelName ?? null,
      payload.gpuMemoryUtilization ?? null,
      payload.maxModelLen ?? null,
      payload.port ?? null,
      payload.gpusPerNode ?? null,
      payload.configJson ?? null,
      payload.gcsTimeoutSecs ?? null,
      payload.readyTimeoutSecs ?? null,
      payload.buildTimeoutSecs ?? null,
      payload.promptPer1k ?? null,
      payload.completionPer1k ?? null,
      payload.audioPerMin ?? null,
      payload.imageEach ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterDeployStopRequest { clusterId, deploymentClusterId } */
  clusterDeployStopRequest(correlationId, { clusterId, deploymentClusterId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterDeployStopRequest(clusterId, deploymentClusterId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ClusterUpdateRequest — wszystkie pola opcjonalne. */
  clusterUpdateRequest(correlationId, opts, sequence = 1) {
    assertReady();
    const {
      clusterId,
      name,
      description,
      strategy,
      failoverEnabled,
      failoverTarget,
      healthCheckIntervalMs,
      timeoutMs,
    } = opts;
    const body = _wasm.encodeClusterUpdateRequest(
      clusterId,
      name ?? null,
      description ?? null,
      strategy ?? null,
      failoverEnabled ?? null,
      failoverTarget ?? null,
      healthCheckIntervalMs ?? null,
      timeoutMs ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Dashboard
  // -------------------------------------------------------------------------

  /** MessageBody::DashboardMetricsRequest (unit). */
  dashboardMetricsRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeDashboardMetricsRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Mesh
  // -------------------------------------------------------------------------

  /** MessageBody::MeshPeersListRequest (unit). */
  meshPeersListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshPeersListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MeshPairInitRequest { nodeId: Uint8Array(32), pin } */
  meshPairInitRequest(correlationId, { nodeId, pin }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeshPairInitRequest(nodeId, pin);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Models
  // -------------------------------------------------------------------------

  /** MessageBody::ModelDetailRequest { modelId } */
  modelDetailRequest(correlationId, { modelId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelDetailRequest(modelId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::CatalogListRequest — unified catalog (service models +
   * published flows + aliases) used by `/v1/models`, mesh `catalog.list`,
   * and the GUI. Pass `surfaceFilter` to narrow by service surface
   * ("chat" | "stt" | …); set `includeBlockingDiagnostics` only when an
   * admin view needs to see hidden RemoteShadowed / LocalOverride entries.
   */
  catalogListRequest(correlationId, sequence = 1, opts = {}) {
    assertReady();
    const surfaceFilter =
      typeof opts.surfaceFilter === 'string' ? opts.surfaceFilter : undefined;
    const includeBlocking = !!opts.includeBlockingDiagnostics;
    const body = _wasm.encodeCatalogListRequest(surfaceFilter, includeBlocking);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelAliasListRequest — lista aliasow modeli. */
  modelAliasListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelAliasListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelAliasCreateRequest { alias, targetModel, strategy?, fallbackTargets? } */
  modelAliasCreateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelAliasCreateRequest(
      payload.alias,
      payload.targetModel,
      payload.strategy ?? null,
      payload.fallbackTargets ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelAliasUpdateRequest { id, alias, targetModel, isActive?, strategy?, fallbackTargets? } */
  modelAliasUpdateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelAliasUpdateRequest(
      Number(payload.id),
      payload.alias,
      payload.targetModel,
      payload.isActive ?? null,
      payload.strategy ?? null,
      payload.fallbackTargets ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelAliasDeleteRequest { id } */
  modelAliasDeleteRequest(correlationId, { id }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelAliasDeleteRequest(Number(id));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AliasConsumerListRequest { aliasId } */
  aliasConsumerListRequest(correlationId, { aliasId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAliasConsumerListRequest(Number(aliasId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AliasConsumerGrantRequest { aliasId, addonId } */
  aliasConsumerGrantRequest(correlationId, { aliasId, addonId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAliasConsumerGrantRequest(Number(aliasId), addonId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AliasConsumerRevokeRequest { aliasId, addonId } */
  aliasConsumerRevokeRequest(correlationId, { aliasId, addonId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAliasConsumerRevokeRequest(Number(aliasId), addonId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AliasVisibilitySetRequest { aliasId, visibility } */
  aliasVisibilitySetRequest(correlationId, { aliasId, visibility }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAliasVisibilitySetRequest(Number(aliasId), visibility);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelVisibilityListRequest — all models with their visibility. */
  modelVisibilityListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelVisibilityListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelVisibilitySetRequest { modelId, visibility } */
  modelVisibilitySetRequest(correlationId, { modelId, visibility }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelVisibilitySetRequest(modelId, visibility);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelConsumerListRequest { modelId } */
  modelConsumerListRequest(correlationId, { modelId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelConsumerListRequest(modelId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelConsumerGrantRequest { modelId, addonId } */
  modelConsumerGrantRequest(correlationId, { modelId, addonId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelConsumerGrantRequest(modelId, addonId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelConsumerRevokeRequest { modelId, addonId } */
  modelConsumerRevokeRequest(correlationId, { modelId, addonId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelConsumerRevokeRequest(modelId, addonId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonAccessListRequest { addonId } */
  addonAccessListRequest(correlationId, { addonId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonAccessListRequest(addonId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonAccessDecisionRequest { addonId, kind, target, decision } */
  addonAccessDecisionRequest(correlationId, { addonId, kind, target, decision }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonAccessDecisionRequest(addonId, kind, target, decision);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelInstallRequest { modelId, sourceRepo } */
  modelInstallRequest(correlationId, { modelId, sourceRepo }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelInstallRequest(modelId, sourceRepo);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelDeleteRequest { modelId } */
  modelDeleteRequest(correlationId, { modelId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelDeleteRequest(modelId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Camera admin (F2 P7.a-bis) — ONVIF wizard
  // -------------------------------------------------------------------------

  /** MessageBody::CameraAdminBody(DiscoverRequest) — start ONVIF WS-Discovery. */
  cameraDiscoverRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCameraDiscoverRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::CameraAdminBody(AddOnvifRequest) — bind a discovered ONVIF
   * device as a managed camera session.
   * payload: { displayName, deviceServiceUrl, username, password,
   *            profileToken?, targetFps? }
   */
  cameraAddOnvifRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCameraAddOnvifRequest(
      payload.displayName,
      payload.deviceServiceUrl,
      payload.username,
      payload.password,
      payload.profileToken ?? null,
      payload.targetFps != null ? Number(payload.targetFps) : null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::CameraAdminBody(FrameUrlRequest) — live-preview tile URL.
   * payload: { cameraId, ttlSecs }
   */
  cameraFrameUrlRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCameraFrameUrlRequest(
      payload.cameraId,
      Number(payload.ttlSecs),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::CameraAdminBody(DetectionsSubscribeRequest) — open a per-camera
   * detection overlay stream. The server replies with a long-lived stream of
   * CameraDetectionsFrame chunks (IS_STREAM_CHUNK) on the same correlation id
   * until MetaCancelStream or disconnect.
   * payload: { cameraId }
   */
  cameraDetectionsSubscribeRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCameraDetectionsSubscribeRequest(payload.cameraId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Robots core app (MessageBody::RobotsBody)
  // -------------------------------------------------------------------------

  /** MessageBody::RobotsBody(ListRequest) — org-scoped robot list (unit). */
  robotsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRobotsListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::RobotsBody(ControlRequest) — route a typed, allowlisted action
   * to a robot (local or over the mesh). `kind` is one of the closed allowlist
   * (move/stop/estop/reset_estop/recovery_stand/stand_up/stand_down/sit/hello/
   * stretch/status); vx/vy/vyaw are normalized -1..1 and only used for "move".
   * payload: { robotId, kind, vx, vy, vyaw, p1, p2, p3, p4 }
   * p1..p4 are generic params keyed by kind (euler → roll/pitch/yaw;
   * body_height/foot_raise_height → p1=height; speed_level → p1=level;
   * pose → roll/pitch/yaw/height). The owner clamps every numeric param.
   */
  robotControlRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRobotControlRequest(
      payload.robotId,
      payload.kind,
      Number(payload.vx ?? 0),
      Number(payload.vy ?? 0),
      Number(payload.vyaw ?? 0),
      Number(payload.p1 ?? 0),
      Number(payload.p2 ?? 0),
      Number(payload.p3 ?? 0),
      Number(payload.p4 ?? 0),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::RobotsBody(CameraShareRequest) — expose a robot's camera to
   * TentaVision (local: persists a cross-addon read grant; remote: view-only).
   * payload: { robotId, cameraId }
   */
  robotCameraShareRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRobotCameraShareRequest(
      payload.robotId,
      payload.cameraId,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::RobotsBody(GeoAnchorSetRequest) — pin/clear the robot's scene
   * origin in the real world. payload: { robotId, lat, lon, alt, heading } (all
   * present = set; all null/undefined = clear).
   */
  robotGeoAnchorSetRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const num = (v) => (v == null ? undefined : Number(v));
    const body = _wasm.encodeRobotGeoAnchorSetRequest(
      payload.robotId,
      num(payload.lat),
      num(payload.lon),
      num(payload.alt),
      num(payload.heading),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::RobotsBody(GeoAnchorGetRequest) — read the robot's geo anchor +
   * live real-world position. payload: { robotId }
   */
  robotGeoAnchorGetRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRobotGeoAnchorGetRequest(payload.robotId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Hub
  // -------------------------------------------------------------------------

  /** MessageBody::HubEngineListRequest (unit). */
  hubEngineListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeHubEngineListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::HubModelSearchRequest { query } */
  hubModelSearchRequest(correlationId, { query }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeHubModelSearchRequest(query);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Flows
  // -------------------------------------------------------------------------

  /** MessageBody::FlowListRequest (unit). */
  flowListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::FlowDetailRequest { flowId } */
  flowDetailRequest(correlationId, { flowId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowDetailRequest(String(flowId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::FlowCreateRequest { name, description, graphJson,
   * publishedModelName? }. `publishedModelName` advertises the flow on
   * `/v1/models` once the catalog rebuilds; the handler rejects names
   * that collide with active aliases or other published flows.
   */
  flowCreateRequest(
    correlationId,
    { name, description, graphJson, publishedModelName },
    sequence = 1,
  ) {
    assertReady();
    const body = _wasm.encodeFlowCreateRequest(
      name,
      description ?? null,
      graphJson,
      publishedModelName ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::FlowDeleteRequest { flowId } */
  flowDeleteRequest(correlationId, { flowId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowDeleteRequest(String(flowId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::FlowExecutionsListRequest { flowId } */
  flowExecutionsListRequest(correlationId, { flowId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowExecutionsListRequest(String(flowId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::FlowUpdateRequest — partial update flow.
   * Pass `publishedModelName: "..."` to publish, `publishedModelName: null`
   * to un-publish. Omit the key (or pass `undefined`) to keep the existing
   * value untouched — `publishSet` is auto-derived from key presence.
   */
  flowUpdateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const publishSet = Object.prototype.hasOwnProperty.call(
      payload,
      'publishedModelName',
    );
    const body = _wasm.encodeFlowUpdateRequest(
      String(payload.flowId),
      payload.name ?? null,
      payload.description ?? null,
      payload.flowJson ?? null,
      payload.status ?? null,
      publishSet,
      publishSet ? payload.publishedModelName ?? null : null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::FlowNodeTemplatesListRequest (unit). */
  flowNodeTemplatesListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowNodeTemplatesListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::FlowVersionListRequest { flowId } */
  flowVersionListRequest(correlationId, { flowId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowVersionListRequest(String(flowId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::FlowVersionGetRequest { flowId, versionId } */
  flowVersionGetRequest(correlationId, { flowId, versionId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowVersionGetRequest(String(flowId), String(versionId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::FlowVersionRestoreRequest { flowId, versionId } */
  flowVersionRestoreRequest(correlationId, { flowId, versionId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowVersionRestoreRequest(String(flowId), String(versionId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Services
  // -------------------------------------------------------------------------

  /** MessageBody::ServiceListRequest (unit). */
  serviceListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ServiceFlagsUpdateRequest { serviceId, pinned?, paused? }
   *  pinned/paused: undefined/null = nie zmieniaj, true/false = ustaw. */
  serviceFlagsUpdateRequest(correlationId, { serviceId, pinned, paused }, sequence = 1) {
    assertReady();
    const pinnedI32 = pinned === undefined || pinned === null ? -1 : (pinned ? 1 : 0);
    const pausedI32 = paused === undefined || paused === null ? -1 : (paused ? 1 : 0);
    const body = _wasm.encodeServiceFlagsUpdateRequest(serviceId, pinnedI32, pausedI32);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::DeploymentBody(ReqRedeploy { serviceId }) */
  serviceRedeployRequest(correlationId, { serviceId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceRedeployRequest(Number(serviceId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ServiceDeployRequest { engineId, modelId, deployMethod, nodeId: Uint8Array(32) } */
  serviceDeployRequest(correlationId, { engineId, modelId, deployMethod, nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceDeployRequest(engineId, modelId, deployMethod, nodeId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::ServiceCreateRequest { name, serviceType, strategy, configJson,
   *   nodeId?, clusterId? }
   * `nodeId` jest hex-enkodowanym 64-znakowym ciagiem (32 bajty) lub pusty.
   */
  serviceCreateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceCreateRequest(
      payload.name,
      payload.serviceType,
      payload.strategy ?? 'single',
      payload.configJson,
      payload.nodeId ?? null,
      payload.clusterId ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::ServiceUpdateRequest { id, name, serviceType, strategy, status,
   *   configJson, nodeId?, clusterId? }
   */
  serviceUpdateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceUpdateRequest(
      String(payload.id),
      payload.name,
      payload.serviceType,
      payload.strategy ?? 'single',
      payload.status ?? 'active',
      payload.configJson,
      payload.nodeId ?? null,
      payload.clusterId ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::ServiceBody(ServicePayload::ReqUpdate) — edycja serwisu
   * po deploy. Payload to JSON-encoded ServiceUpdateRequest (13 pól
   * opcjonalnych — łatwiej tak niż 13 args wasm-bindgen).
   */
  serviceConfigUpdateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceConfigUpdateRequest(JSON.stringify(camelToSnakePayload(payload)));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqVramHint) */
  serviceVramHintRequest(correlationId, { gpuIndex, nodeId, excludeServiceId } = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceVramHintRequest(
      gpuIndex == null ? undefined : Number(gpuIndex),
      nodeId ?? undefined,
      excludeServiceId == null ? undefined : Number(excludeServiceId),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqEnginePresets) */
  serviceEnginePresetsRequest(correlationId, { engineId } = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceEnginePresetsRequest(String(engineId || ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqModelCatalog) */
  serviceModelCatalogRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceModelCatalogRequest(JSON.stringify(camelToSnakePayload(payload)));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqModelSelection) */
  serviceModelSelectionRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceModelSelectionRequest(JSON.stringify(camelToSnakePayload(payload)));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqOauthStart) */
  serviceOauthStartRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceOauthStartRequest(JSON.stringify(camelToSnakePayload(payload)));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqOauthPoll) */
  serviceOauthPollRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceOauthPollRequest(JSON.stringify(camelToSnakePayload(payload)));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body,
    );
  },

  /** MessageBody::ServiceQuicStatusRequest (unit) */
  serviceQuicStatusRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceQuicStatusRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Prompts
  // -------------------------------------------------------------------------

  /** MessageBody::PromptListRequest (unit). */
  promptListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodePromptListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::PromptDetailRequest { promptId } */
  promptDetailRequest(correlationId, { promptId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodePromptDetailRequest(promptId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Meeting Bot
  // -------------------------------------------------------------------------

  // Deployment status/list polling.
  deploymentStatusRequest(correlationId, { deployId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeDeploymentStatusRequest(String(deployId || ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  deploymentListRequest(correlationId, { engineId = '', status = '', onlyMine = true, limit = 0 } = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeDeploymentListRequest(String(engineId), String(status), !!onlyMine, Number(limit));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** Subscribe streaming log/progress — wywołuj przez ApiBinary.subscribe(...) */
  deploymentLogStreamRequest(correlationId, { deployId, replayTail = true }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeDeploymentLogStreamRequest(String(deployId || ''), !!replayTail);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** Subscribe — otwiera tunel RFB dla sesji meeting, chunki to RFB bytes z kontenera. */
  vncTunnelOpenRequest(correlationId, { sessionId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeVncTunnelOpenRequest(Number(sessionId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** One-shot — wysyła RFB input (keyboard/mouse) z przeglądarki do kontenera. */
  vncTunnelSendRequest(correlationId, { tunnelId, bytes }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeVncTunnelSendRequest(String(tunnelId), bytes);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** One-shot — zamyka tunel RFB i zwalnia zasoby po stronie backendu. */
  vncTunnelCloseRequest(correlationId, { tunnelId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeVncTunnelCloseRequest(String(tunnelId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** One-shot — capture screenshot (PNG) or DOM (HTML) from the bot's Chromium page. */
  browserCaptureRequest(correlationId, { sessionId, kind, fullPage = false }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBrowserCaptureRequest(Number(sessionId), String(kind), !!fullPage);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  meetingSessionStartRequest(correlationId, { meetingUrl, title, platform, botName, sttAlias, ttsAlias, llmAlias }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeetingSessionStartRequest(
      meetingUrl ?? '',
      title ?? '',
      platform ?? 'teams',
      botName ?? '',
      sttAlias ?? '',
      ttsAlias ?? '',
      llmAlias ?? '',
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  meetingSessionLeaveRequest(correlationId, { sessionId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeetingSessionLeaveRequest(Number(sessionId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  meetingSessionListRequest(correlationId, { onlyMine } = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeetingSessionListRequest(!!onlyMine);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  meetingSessionDetailRequest(correlationId, { sessionId, includeTranscripts }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeetingSessionDetailRequest(Number(sessionId), !!includeTranscripts);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  meetingTranscriptsListRequest(correlationId, { sessionId, sinceMs = 0 }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeetingTranscriptsListRequest(Number(sessionId), Number(sinceMs));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  meetingActiveSessionRequest(correlationId, _args, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeetingActiveSessionRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  meetingSettingsGetRequest(correlationId, _args, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeetingSettingsGetRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** settings: Record<string,string> or Array<[key,value]> */
  meetingSettingsUpdateRequest(correlationId, { settings }, sequence = 1) {
    assertReady();
    const pairs = Array.isArray(settings) ? settings : Object.entries(settings ?? {});
    const body = _wasm.encodeMeetingSettingsUpdateRequest(pairs);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MeetingSummariesListRequest { meeting_key, limit? } — lista najnowszych podsumowan. */
  meetingSummariesListRequest(correlationId, { meetingKey, limit } = {}, sequence = 1) {
    assertReady();
    const lim = limit == null ? undefined : Number(limit);
    const body = _wasm.encodeMeetingSummariesListRequest(String(meetingKey ?? ''), lim);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MeetingActionItemsListRequest { meeting_key, status_filter? } */
  meetingActionItemsListRequest(correlationId, { meetingKey, statusFilter } = {}, sequence = 1) {
    assertReady();
    const sf = statusFilter == null || statusFilter === '' ? undefined : String(statusFilter);
    const body = _wasm.encodeMeetingActionItemsListRequest(String(meetingKey ?? ''), sf);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MeetingActionItemStatusUpdateRequest { item_id, status } */
  meetingActionItemStatusUpdateRequest(correlationId, { itemId, status } = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeetingActionItemStatusUpdateRequest(Number(itemId), String(status ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MeetingTranscriptExportRequest { meeting_key } — zwraca plain text w polu content. */
  meetingTranscriptExportRequest(correlationId, { meetingKey } = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMeetingTranscriptExportRequest(String(meetingKey ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  // -------------------------------------------------------------------------
  // Registries
  // -------------------------------------------------------------------------

  /** MessageBody::RegistryListRequest (unit). */
  registryListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRegistryListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // TTS rules
  // -------------------------------------------------------------------------

  /** MessageBody::TtsRuleListRequest (unit). */
  ttsRuleListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTtsRuleListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::TtsRuleCreateRequest(TtsRule) */
  ttsRuleCreateRequest(correlationId, { id, pattern, voiceId, priority }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTtsRuleCreateRequest(id, pattern, voiceId, priority);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::TtsRuleDeleteRequest { ruleId } */
  ttsRuleDeleteRequest(correlationId, { ruleId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTtsRuleDeleteRequest(ruleId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::TtsPreviewRequest { text, model, voice } — podglad audio TTS */
  ttsPreviewRequest(correlationId, { text, model, voice }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTtsPreviewRequest(text, model, voice);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // PII rules
  // -------------------------------------------------------------------------

  /** MessageBody::PiiRuleListRequest (unit). */
  piiRuleListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodePiiRuleListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::VisionBody(InferRequest). Dwa formaty obrazka:
   *   - encoded JPEG/PNG/WEBP: podajesz tylko `image` (Uint8Array), bez width/height.
   *   - raw RGB row-major: podajesz `image` + `width` + `height`.
   *
   * @param {string} correlationId
   * @param {{ serviceName: string, image: Uint8Array, width?: number, height?: number }} args
   * @param {number} sequence
   */
  visionInferRequest(correlationId, { serviceName, image, width, height }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeVisionInferRequest(
      serviceName,
      image,
      typeof width === 'number' ? width : undefined,
      typeof height === 'number' ? height : undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::RerankBody(Request). Natywny rerank (Tier 1) — odpowiednik
   * REST /v1/rerank. `topN` opcjonalne (pomiń = wszystkie dokumenty).
   *
   * @param {string} correlationId
   * @param {{ model: string, query: string, documents: string[], topN?: number, returnDocuments?: boolean }} args
   * @param {number} sequence
   */
  rerankRequest(correlationId, { model, query, documents, topN, returnDocuments }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRerankRequest(
      model,
      query,
      documents,
      typeof topN === 'number' ? topN : undefined,
      returnDocuments === true,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Fast-path patterns
  // -------------------------------------------------------------------------

  /** MessageBody::FastPathListRequest (unit). */
  fastPathListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFastPathListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Settings
  // -------------------------------------------------------------------------

  /** MessageBody::SettingsListRequest (unit). */
  settingsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSettingsListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::SettingsUpdateRequest { entries: [{key, value, isSecret}] }
   * Przekazywane jako trzy rownolegle tablice do WASM (no serde-wasm-bindgen).
   */
  settingsUpdateRequest(correlationId, { entries }, sequence = 1) {
    assertReady();
    const keys = entries.map((e) => String(e.key));
    const values = entries.map((e) => String(e.value));
    const isSecrets = new Uint8Array(entries.map((e) => (e.isSecret ? 1 : 0)));
    const body = _wasm.encodeSettingsUpdateBatch(keys, values, isSecrets);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Storage admin (Ustawienia → Magazyn danych)
  // -------------------------------------------------------------------------

  /** MessageBody::StorageAdminBody(OverviewRequest) — unit. */
  storageOverviewRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeStorageOverviewRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::StorageAdminBody(BrowseRequest { path }). */
  storageBrowseRequest(correlationId, { path }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeStorageBrowseRequest(String(path ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::StorageAdminBody(CreateDirRequest { parent, name }). */
  storageCreateDirRequest(correlationId, { parent, name }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeStorageCreateDirRequest(String(parent), String(name));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::StorageAdminBody(MigrateRequest { key, newPath, moveData }). */
  storageMigrateRequest(correlationId, { key, newPath, moveData }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeStorageMigrateRequest(String(key), String(newPath), moveData === true);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Network (interfejsy hosta + konfiguracja bind/filter mesh)
  // -------------------------------------------------------------------------

  /** MessageBody::NetworkBody(NetworkPayload::ReqInterfacesList) — unit. */
  networkInterfacesListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeNetworkInterfacesListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::NetworkBody(NetworkPayload::ReqConfigGet) — unit. */
  networkConfigGetRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeNetworkConfigGetRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::NetworkBody(NetworkPayload::ReqRelayStatus) — unit. */
  networkRelayStatusRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeNetworkRelayStatusRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::NetworkBody(NetworkPayload::ReqConfigUpdate(NetworkConfig)).
   * `payload` akceptuje pola w camelCase lub snake_case (alias), co upraszcza
   * integracje z istniejacym kodem GUI.
   */
  networkConfigUpdateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const bindMode = String(payload.bindMode ?? payload.bind_mode ?? 'auto');
    const bindIpv4 = String(payload.bindIpv4 ?? payload.bind_ipv4 ?? '');
    const hideDocker = !!(payload.hideDocker ?? payload.hide_docker);
    const hideLinkLocal = !!(payload.hideLinkLocal ?? payload.hide_link_local);
    const hideLoopback = !!(payload.hideLoopback ?? payload.hide_loopback);
    const hideCgnat = !!(payload.hideCgnat ?? payload.hide_cgnat);
    const preferSameSubnet = !!(payload.preferSameSubnet ?? payload.prefer_same_subnet);
    const irohRelayUrl = String(payload.irohRelayUrl ?? payload.iroh_relay_url ?? '');
    const excludedRaw = payload.excludedInterfaces ?? payload.excluded_interfaces ?? [];
    const excludedInterfaces = Array.isArray(excludedRaw) ? excludedRaw.map(String) : [];
    const body = _wasm.encodeNetworkConfigUpdateRequest(
      bindMode,
      bindIpv4,
      hideDocker,
      hideLinkLocal,
      hideLoopback,
      hideCgnat,
      preferSameSubnet,
      irohRelayUrl,
      excludedInterfaces,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Multi-source profiling — ProfilingPayload w MessageBody::ProfilingBody.
  // `scope` musi byc obiektem zgodnym z ProfileScope:
  //   { sources: u32, gpuTargets: 'all'|'none'|{indices:[..]}|{byVendor:'nvidia'},
  //     cpuSamplingHz: u32, target: 'system_wide'|'own_process'|{pid:u32},
  //     durationSeconds: u32, label: string }
  // -------------------------------------------------------------------------

  /** MessageBody::ProfilingBody(ProfilingPayload::StartRequest). */
  profilingStartRequest(
    correlationId,
    { nodeId, scope, label, elevationPassword },
    sequence = 1,
  ) {
    assertReady();
    const body = _wasm.encodeProfilingStartRequest(
      String(nodeId),
      scope,
      String(label ?? ''),
      elevationPassword == null ? undefined : String(elevationPassword),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ProfilingBody(ProfilingPayload::StopRequest). */
  profilingStopRequest(correlationId, { nodeId, sessionId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProfilingStopRequest(String(nodeId), String(sessionId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ProfilingBody(ProfilingPayload::SessionsRequest). */
  profilingSessionsRequest(correlationId, { nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProfilingSessionsRequest(String(nodeId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ProfilingBody(ProfilingPayload::ReportRequest). */
  profilingReportRequest(correlationId, { nodeId, sessionId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProfilingReportRequest(String(nodeId), String(sessionId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ProfilingBody(ProfilingPayload::DeleteRequest). */
  profilingDeleteRequest(correlationId, { nodeId, sessionId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProfilingDeleteRequest(String(nodeId), String(sessionId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ProfilingBody(ProfilingPayload::DownloadRequest). */
  profilingDownloadRequest(correlationId, { nodeId, sessionId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProfilingDownloadRequest(String(nodeId), String(sessionId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ProfilingBody(ProfilingPayload::ActiveInfoRequest). */
  profilingActiveInfoRequest(correlationId, { nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProfilingActiveInfoRequest(String(nodeId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** ProfilingPayload::ValidateSudoRequest — sudo password (used once, never logged). */
  profilingValidateSudoRequest(correlationId, { nodeId, password }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProfilingValidateSudoRequest(
      String(nodeId ?? ''),
      String(password ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** ProfilingPayload::CollectorsStatusRequest — list collectors + binary paths. */
  profilingCollectorsStatusRequest(correlationId, { nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProfilingCollectorsStatusRequest(String(nodeId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // SSO / TLS / NGC (FAZA 4)
  // -------------------------------------------------------------------------

  /** MessageBody::SsoProvidersListRequest (unit). */
  ssoProvidersListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSsoProvidersListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::SsoProviderCreateRequest — pelne dane providera. */
  ssoProviderCreateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSsoProviderCreateRequest(
      String(payload.name ?? ''),
      String(payload.providerType ?? ''),
      String(payload.clientId ?? ''),
      String(payload.clientSecret ?? ''),
      String(payload.discoveryUrl ?? ''),
      !!payload.autoCreateUsers,
      payload.defaultGroupId == null ? undefined : String(payload.defaultGroupId),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::SsoProviderDeleteRequest { id }. */
  ssoProviderDeleteRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSsoProviderDeleteRequest(Number(payload.id));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::TlsStatusRequest (unit). */
  tlsStatusRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTlsStatusRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::NgcStatusRequest (unit). */
  ngcStatusRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeNgcStatusRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // -------------------------------------------------------------------------
  // Katalog: NIM + manifest deploy (FAZA 5)
  // -------------------------------------------------------------------------

  /** MessageBody::NimCatalogListRequest (unit). */
  nimCatalogListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeNimCatalogListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::ServiceManifestDeployRequest { engineId, deployMethod, nodeId, configJson }.
   * `configJson` jest stringify'owanym JSON-em z wizarda (model preset, port itp.).
   */
  serviceManifestDeployRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceManifestDeployRequest(
      String(payload.engineId ?? ''),
      String(payload.deployMethod ?? ''),
      String(payload.nodeId ?? ''),
      String(payload.configJson ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonsListRequest (unit) — lista zainstalowanych addonow. */
  addonsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonsListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonUiBody(ReqApplicationsList) — lista aplikacji addonow
   *  do glownego menu launcher. Frontend buduje liste ikon. */
  addonApplicationsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonApplicationsListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonInstanceBody(ReqCatalogList) — lista pakietow w katalogu. */
  addonCatalogListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonCatalogListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonInstanceBody(ReqInstall) — instalacja instancji z katalogu. */
  addonInstanceInstallRequest(
    correlationId,
    { packageId, version, displayName, config = [] },
    sequence = 1,
  ) {
    assertReady();
    const body = _wasm.encodeAddonInstanceInstallRequest(packageId, version, displayName, config);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonInstanceBody(ReqDuplicate) — duplikacja instancji. */
  addonInstanceDuplicateRequest(
    correlationId,
    { sourceAddonId, newDisplayName },
    sequence = 1,
  ) {
    assertReady();
    const body = _wasm.encodeAddonInstanceDuplicateRequest(sourceAddonId, newDisplayName);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonInstanceBody(ReqVersions) — wersje dostepne dla instancji. */
  addonInstanceVersionsRequest(correlationId, { addonId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonInstanceVersionsRequest(addonId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonInstanceBody(ReqUpdate) — hot-update instancji do wersji. */
  addonInstanceUpdateRequest(correlationId, { addonId, targetVersion }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonInstanceUpdateRequest(addonId, targetVersion);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonStorageBody(StatsRequest) — statystyki storage addona. */
  addonStorageStatsRequest(correlationId, { addonId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonStorageStatsRequest(addonId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonVectorBody(GetConfigRequest) — config vector backendu. */
  addonVectorGetConfigRequest(correlationId, { addonId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonVectorGetConfigRequest(addonId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonVectorBody(SetConfigRequest) — zapis config vector backendu. */
  addonVectorSetConfigRequest(
    correlationId,
    {
      addonId,
      backend,
      milvusSource,
      serviceNodeId,
      serviceId,
      manualUri,
      collectionOverride,
      milvusUser,
      milvusPassword,
    },
    sequence = 1,
  ) {
    assertReady();
    const body = _wasm.encodeAddonVectorSetConfigRequest(
      addonId,
      backend,
      milvusSource ?? undefined,
      serviceNodeId ?? undefined,
      serviceId ?? undefined,
      manualUri ?? undefined,
      collectionOverride ?? undefined,
      milvusUser ?? undefined,
      milvusPassword ?? undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },


  /** MessageBody::UsersListRequest (unit, Admin) — lista uzytkownikow. */
  usersListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeUsersListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::AuditLogListRequest — Admin. Lista logow audytowych z
   * filtrami + paginacja. Wszystkie pola filter sa optional.
   * payload: { userId?, addonId?, action?, fromDate?, toDate?, search?, offset?, limit? }
   */
  auditLogListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAuditLogListRequest(
      payload.userId == null ? null : String(payload.userId),
      payload.addonId ?? null,
      payload.action ?? null,
      payload.fromDate ?? null,
      payload.toDate ?? null,
      payload.search ?? null,
      Number(payload.offset ?? 0),
      Number(payload.limit ?? 100) >>> 0,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  schedulerJobsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSchedulerJobsListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  schedulerActionsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSchedulerActionsListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  schedulerRunsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSchedulerRunsListRequest(
      String(payload.jobId ?? payload.job_id ?? ''),
      Number(payload.limit ?? 20) >>> 0,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  schedulerJobUpsertRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSchedulerJobUpsertRequest(
      typeof payload.jobJson === 'string' ? payload.jobJson : JSON.stringify(payload.job ?? payload),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  schedulerJobDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSchedulerJobDeleteRequest(String(payload.jobId ?? payload.job_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  schedulerJobRunNowRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSchedulerJobRunNowRequest(String(payload.jobId ?? payload.job_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::TokenUsageBody(UsageSummaryRequest) — agregat zuzycia tokenow. */
  tokenUsageSummaryRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTokenUsageSummaryRequest(
      String(payload.period ?? 'daily'),
      String(payload.periodKey ?? payload.period_key ?? ''),
      String(payload.groupBy ?? payload.group_by ?? 'user'),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::TokenUsageBody(ListQuotasRequest) — lista limitow org. */
  tokenListQuotasRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTokenListQuotasRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::TokenUsageBody(UpsertQuotaRequest) — utworz/aktualizuj limit. */
  tokenUpsertQuotaRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const quota = payload.quota ?? payload;
    const body = _wasm.encodeTokenUpsertQuotaRequest(
      quota.id ?? null,
      String(quota.scopeType ?? quota.scope_type ?? ''),
      quota.subjectId ?? quota.subject_id ?? null,
      quota.modelId ?? quota.model_id ?? null,
      String(quota.period ?? 'daily'),
      BigInt(quota.maxTotalTokens ?? quota.max_total_tokens ?? 0),
      Boolean(quota.isActive ?? quota.is_active ?? true),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::TokenUsageBody(DeleteQuotaRequest) — usun limit po id. */
  tokenDeleteQuotaRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTokenDeleteQuotaRequest(String(payload.id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::TokenUsageBody(CoordinatorStatusRequest) — status koordynatora. */
  tokenCoordinatorStatusRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeTokenCoordinatorStatusRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelMetricsBody(SummaryRequest) — mesh-wide rollup agregacji. */
  modelMetricsSummaryRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const nz = (v) => {
      const s = v == null ? '' : String(v);
      return s === '' ? undefined : s;
    };
    const body = _wasm.encodeModelMetricsSummaryRequest(
      String(payload.period ?? 'daily'),
      String(payload.periodKey ?? payload.period_key ?? ''),
      String(payload.groupBy ?? payload.group_by ?? 'user'),
      nz(payload.filterModel ?? payload.filter_model ?? payload.model),
      nz(payload.filterNode ?? payload.filter_node ?? payload.node),
      nz(payload.filterService ?? payload.filter_service ?? payload.service),
      nz(payload.filterBackend ?? payload.filter_backend ?? payload.backend),
      nz(payload.filterModality ?? payload.filter_modality ?? payload.modality),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelMetricsBody(NodeServiceRequest) — przekrój węzeł×serwis. */
  modelMetricsNodeServiceRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelMetricsNodeServiceRequest(
      String(payload.period ?? 'daily'),
      String(payload.periodKey ?? payload.period_key ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelMetricsBody(PricingGet) — odczyt cennika per-model. */
  modelMetricsPricingGet(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelMetricsPricingGet();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ModelMetricsBody(PricingSet) — zapis cennika jednego modelu. */
  modelMetricsPricingSet(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelMetricsPricingSet(
      String(payload.modelId ?? payload.model_id ?? ''),
      Number(payload.promptPer1k ?? payload.prompt_per_1k ?? 0),
      Number(payload.completionPer1k ?? payload.completion_per_1k ?? 0),
      Number(payload.audioPerMin ?? payload.audio_per_min ?? 0),
      Number(payload.imageEach ?? payload.image_each ?? 0),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::BenchmarkBody(ListRequest) — Benchmark Studio overview. */
  benchmarkListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkListRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::BenchmarkBody(GetRequest) — full definition for the editor. payload: { id }. */
  benchmarkGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkGetRequest(String(payload.id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::BenchmarkBody(SaveRequest). payload: { id?, name, configJson, targets:[] }. */
  benchmarkSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const id = payload.id == null || payload.id === '' ? undefined : String(payload.id);
    const targets = Array.isArray(payload.targets) ? payload.targets : [];
    const body = _wasm.encodeBenchmarkSaveRequest(
      id,
      String(payload.name ?? ''),
      String(payload.configJson ?? payload.config_json ?? '{}'),
      JSON.stringify(targets),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::BenchmarkBody(DeleteRequest). payload: { id }. */
  benchmarkDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkDeleteRequest(String(payload.id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::BenchmarkBody(StartRunRequest) — non-blocking, returns runId. payload: { benchmarkId }. */
  benchmarkStartRunRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkStartRunRequest(String(payload.benchmarkId ?? payload.benchmark_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::BenchmarkBody(RunStatusRequest). payload: { runId }. */
  benchmarkRunStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkRunStatusRequest(String(payload.runId ?? payload.run_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::BenchmarkBody(RunResultsRequest). payload: { runId }. */
  benchmarkRunResultsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkRunResultsRequest(String(payload.runId ?? payload.run_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::BenchmarkBody(ListRunsRequest) — run history for one benchmark. payload: { benchmarkId }. */
  benchmarkListRunsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkListRunsRequest(String(payload.benchmarkId ?? payload.benchmark_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::BenchmarkBody(RecentRunsRequest) — newest runs across all benchmarks. */
  benchmarkRecentRunsRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkRecentRunsRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::BenchmarkBody(CancelRunRequest). payload: { runId }. */
  benchmarkCancelRunRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkCancelRunRequest(String(payload.runId ?? payload.run_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** Subscribe — live run progress. payload: { runId }. Chunks = BenchmarkRunStreamChunk. */
  benchmarkRunStreamRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeBenchmarkRunStreamRequest(String(payload.runId ?? payload.run_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::MlStudioBody(ProjectsListRequest) — ML Studio projects list. */
  mlStudioProjectsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectsListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectTypesListRequest) — six fixed project types. */
  mlStudioProjectTypesListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectTypesListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectCreateRequest). payload: { name, description, projectType }. */
  mlStudioProjectCreateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectCreateRequest(
      String(payload.name ?? ''),
      String(payload.description ?? ''),
      String(payload.projectType ?? payload.project_type ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectDetailRequest). payload: { projectId }. */
  mlStudioProjectDetailRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectDetailRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectMembersListRequest). payload: { projectId }. */
  mlStudioProjectMembersListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectMembersListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectInviteRequest). payload: { projectId, inviteeUserId, role }. */
  mlStudioProjectInviteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectInviteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.inviteeUserId ?? payload.invitee_user_id ?? ''),
      String(payload.role ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectMemberRemoveRequest). payload: { projectId, userId }. */
  mlStudioProjectMemberRemoveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectMemberRemoveRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.userId ?? payload.user_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectMemberRoleSetRequest). payload: { projectId, userId, role }. */
  mlStudioProjectMemberRoleSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectMemberRoleSetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.userId ?? payload.user_id ?? ''),
      String(payload.role ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(DatasetUploadRequest). Uploads a tabular file
   * (CSV/XLSX) into a project for profiling. `bytes` must be a Uint8Array of the
   * raw file content — carried inline in the CBOR body (no multipart).
   * payload: { projectId, name, filename, bytes }
   */
  mlStudioDatasetUploadRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const raw = payload.bytes;
    const bytes = raw instanceof Uint8Array ? raw : new Uint8Array(raw ?? []);
    const body = _wasm.encodeMlStudioDatasetUploadRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.name ?? ''),
      String(payload.filename ?? ''),
      bytes,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(DatasetUploadChunkRequest). Jeden fragment dużego
   * pliku — klient dzieli plik na części o numerach seq (0..totalChunks) i wysyła
   * je sekwencyjnie pod wspólnym uploadId. Serwer tworzy dataset po ostatnim
   * fragmencie. payload: { projectId, name, filename, uploadId, seq, totalChunks, bytes }
   */
  mlStudioDatasetUploadChunkRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const raw = payload.bytes;
    const bytes = raw instanceof Uint8Array ? raw : new Uint8Array(raw ?? []);
    const body = _wasm.encodeMlStudioDatasetUploadChunkRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.name ?? ''),
      String(payload.filename ?? ''),
      String(payload.uploadId ?? payload.upload_id ?? ''),
      Number(payload.seq ?? 0),
      Number(payload.totalChunks ?? payload.total_chunks ?? 0),
      bytes,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::AddonDocumentUploadChunkRequestBody. Jeden fragment pliku
   * wgrywanego z panelu UI addona do JEGO document store. Klient dzieli plik na
   * części `seq` (0..totalChunks) pod wspólnym `uploadId`; serwer zwraca `docRef`
   * po ostatnim fragmencie. `org` bierze z sesji — NIE jest polem requestu.
   * payload: { addonId, uploadId, filename, mime, seq, totalChunks, bytes }
   */
  addonDocumentUploadChunkRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const raw = payload.bytes;
    const bytes = raw instanceof Uint8Array ? raw : new Uint8Array(raw ?? []);
    const body = _wasm.encodeAddonDocumentUploadChunkRequest(
      String(payload.addonId ?? payload.addon_id ?? ''),
      String(payload.uploadId ?? payload.upload_id ?? ''),
      String(payload.filename ?? ''),
      String(payload.mime ?? ''),
      Number(payload.seq ?? 0),
      Number(payload.totalChunks ?? payload.total_chunks ?? 0),
      String(payload.source ?? ''),
      bytes,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(DatasetsListRequest). payload: { projectId }. */
  mlStudioDatasetsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioDatasetsListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(DatasetProfileRequest). payload: { datasetId }. */
  mlStudioDatasetProfileRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioDatasetProfileRequest(
      String(payload.datasetId ?? payload.dataset_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(DatasetRowsRequest) — pobranie WIERSZY datasetu
   * (linie JSONL) do podglądu/edycji. payload: { datasetId, limit? }.
   */
  mlStudioDatasetRowsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioDatasetRowsRequest(
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      (payload.limit ?? 0) >>> 0,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(DatasetRowsSaveRequest) — nadpisanie datasetu
   * ręcznie edytowanymi wierszami. payload: { datasetId, rows: string[] (linie JSONL) }.
   */
  mlStudioDatasetRowsSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const rows = Array.isArray(payload.rows) ? payload.rows.map((r) => String(r)) : [];
    const body = _wasm.encodeMlStudioDatasetRowsSaveRequest(
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      rows,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(TabularTrainRequest) — train the tabular baseline
   * on a dataset's target column and get a ranked leaderboard back.
   * payload: { projectId, datasetId, targetColumn, task: 'classification'|'regression', engine?: 'rust'|'autogluon' }.
   */
  mlStudioTabularTrainRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    // engine wybiera silnik treningu (rust domyślny / autogluon przez serwis);
    // pusty string gdy nie podano — backend potraktuje to jak silnik rust.
    const engine = payload.engine ?? payload.engine_id ?? '';
    const body = _wasm.encodeMlStudioTabularTrainRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      String(payload.targetColumn ?? payload.target_column ?? ''),
      String(payload.task ?? 'classification'),
      engine ? String(engine) : undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(ResourceGrantCreateRequest) — Admin allocates a
   * mesh node resource to a subject (§11.3). Pool of nodes comes from the mesh
   * registry (MeshNodeListRequest); this only records the GRANT.
   * payload: { subjectKind: 'user'|'group'|'project', subjectId, nodeId,
   *            resourceKind: 'gpu'|'cpu'|'ram', resourceRef?, quota? }
   */
  mlStudioResourceGrantCreateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioResourceGrantCreateRequest(
      String(payload.subjectKind ?? payload.subject_kind ?? ''),
      String(payload.subjectId ?? payload.subject_id ?? ''),
      String(payload.nodeId ?? payload.node_id ?? ''),
      String(payload.resourceKind ?? payload.resource_kind ?? ''),
      String(payload.resourceRef ?? payload.resource_ref ?? ''),
      String(payload.quota ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ResourceGrantsListRequest) — Admin: all grants. */
  mlStudioResourceGrantsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioResourceGrantsListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ResourceGrantRevokeRequest). payload: { grantId }. */
  mlStudioResourceGrantRevokeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioResourceGrantRevokeRequest(
      String(payload.grantId ?? payload.grant_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(ProjectResourcesRequest) — a project member sees
   * the resources allocated to the project. payload: { projectId }.
   */
  mlStudioProjectResourcesRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectResourcesRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(ModelsListRequest) — lista wytrenowanych modeli
   * projektu (zakładka Modele). payload: { projectId }.
   */
  mlStudioModelsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioModelsListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(TrainingRunsListRequest) — historia treningów
   * projektu (zakładka Treningi). payload: { projectId }.
   */
  mlStudioTrainingRunsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioTrainingRunsListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(ProjectGrantsListRequest) — granty zasobów
   * przydzielone projektowi (member-dostępne). payload: { projectId }.
   */
  mlStudioProjectGrantsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectGrantsListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(FtTrainStartRequest) — startuje ASYNCHRONICZNY
   * fine-tuning LLM. Odpowiedź wraca natychmiast z runId; postęp odpytuj przez
   * mlStudioFtTrainStatusRequest.
   * payload: { projectId, datasetId, baseModel, method, objective,
   *            mergeAdapter?, hyperparams: { learningRate, batchSize,
   *            gradAccumSteps, epochs, loraR, loraAlpha, loraDropout,
   *            maxSeqLen } }
   */
  mlStudioFtTrainStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const hp = payload.hyperparams ?? {};
    const body = _wasm.encodeMlStudioFtTrainStartRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      String(payload.baseModel ?? payload.base_model ?? ''),
      String(payload.method ?? 'lora'),
      String(payload.objective ?? 'sft'),
      (payload.teacherModel ?? payload.teacher_model) || undefined,
      Number(hp.learningRate ?? hp.learning_rate ?? 2e-4),
      (hp.batchSize ?? hp.batch_size ?? 1) >>> 0,
      (hp.gradAccumSteps ?? hp.grad_accum_steps ?? 8) >>> 0,
      (hp.epochs ?? 3) >>> 0,
      (hp.loraR ?? hp.lora_r ?? 16) >>> 0,
      (hp.loraAlpha ?? hp.lora_alpha ?? 32) >>> 0,
      Number(hp.loraDropout ?? hp.lora_dropout ?? 0.05),
      (hp.maxSeqLen ?? hp.max_seq_len ?? 1024) >>> 0,
      Boolean(payload.mergeAdapter ?? payload.merge_adapter ?? false),
      (payload.targetNodeId ?? payload.target_node_id) || undefined,
      (payload.numGpus ?? payload.num_gpus ?? 0) >>> 0,
      (payload.dist?.nnodes ?? 0) >>> 0,
      (payload.dist?.nodeRank ?? payload.dist?.node_rank ?? 0) >>> 0,
      String(payload.dist?.masterAddr ?? payload.dist?.master_addr ?? ''),
      (payload.dist?.masterPort ?? payload.dist?.master_port ?? 29500) >>> 0,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(DistillGenerateRequest) — start generowania
   * datasetu destylacji. payload: { projectId, datasetName, questionSource
   * ('import'|'generate'), sourceDatasetId?, questionField?, generatePrompt?,
   * questionModel?, numQuestions?, teacherModel, answerInstruction?,
   * temperature?, maxTokens? }.
   */
  mlStudioDistillGenerateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioDistillGenerateRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.datasetName ?? payload.dataset_name ?? ''),
      String(payload.questionSource ?? payload.question_source ?? 'generate'),
      (payload.sourceDatasetId ?? payload.source_dataset_id) || undefined,
      (payload.questionField ?? payload.question_field) || undefined,
      (payload.generatePrompt ?? payload.generate_prompt) || undefined,
      (payload.questionModel ?? payload.question_model) || undefined,
      (payload.numQuestions ?? payload.num_questions ?? 0) >>> 0,
      String(payload.teacherModel ?? payload.teacher_model ?? ''),
      (payload.answerInstruction ?? payload.answer_instruction) || undefined,
      Number(payload.temperature ?? 0),
      (payload.maxTokens ?? payload.max_tokens ?? 0) >>> 0,
      (payload.objective) || undefined,
      (payload.rejectedModel ?? payload.rejected_model) || undefined,
      (payload.rejectedInstruction ?? payload.rejected_instruction) || undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(DistillGenerateStatusRequest) — polling postępu
   * generowania datasetu destylacji. payload: { datasetId }.
   */
  mlStudioDistillGenerateStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioDistillGenerateStatusRequest(
      String(payload.datasetId ?? payload.dataset_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(FtTrainStatusRequest) — polling postępu
   * fine-tuningu LLM. Zwraca status + krzywą straty (lossCurve) do wykresu.
   * payload: { runId }.
   */
  mlStudioFtTrainStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioFtTrainStatusRequest(
      String(payload.runId ?? payload.run_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecogTrainStartRequest) — startuje ASYNCHRONICZNY
   * trening detekcji RF-DETR na datasecie COCO. Odpowiedź natychmiast z runId;
   * postęp (epoka, loss, mAP@50) odpytuj przez mlStudioRecogTrainStatusRequest.
   * payload: { projectId, datasetId, variant, hyperparams }.
   */
  mlStudioRecogTrainStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const hp = payload.hyperparams ?? {};
    const body = _wasm.encodeMlStudioRecogTrainStartRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      String(payload.variant ?? 'base'),
      (hp.epochs ?? 50) >>> 0,
      (hp.batchSize ?? hp.batch_size ?? 4) >>> 0,
      (hp.gradAccum ?? hp.grad_accum ?? 4) >>> 0,
      Number(hp.learningRate ?? hp.learning_rate ?? 1e-4),
      (hp.resolution ?? 560) >>> 0,
      Boolean(hp.earlyStopping ?? hp.early_stopping ?? true),
      (payload.targetNodeId ?? payload.target_node_id) || undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecogTrainStatusRequest) — polling postępu
   * treningu detekcji. Zwraca status + krzywą (epoch, train_loss, map50).
   * payload: { runId }.
   */
  mlStudioRecogTrainStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogTrainStatusRequest(
      String(payload.runId ?? payload.run_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(ClassifierTrainStartRequest) — startuje
   * ASYNCHRONICZNY trening KLASYFIKATORA ATRYBUTU na wycinkach (np. atrybut
   * "stan" o wartościach czysta/brudna). Cropy buduje serwis Python. Odpowiedź
   * natychmiast z runId; postęp odpytuj przez mlStudioGenericTrainStatusRequest.
   * payload: { projectId, datasetId, attribute, sourceClass, variant, values,
   * hyperparams:{epochs,batchSize,learningRate,imageSize,freezeBackbone},
   * targetNodeId }.
   */
  mlStudioClassifierTrainStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const hp = payload.hyperparams ?? {};
    const values = (payload.values ?? []).map((v) => String(v));
    const body = _wasm.encodeMlStudioClassifierTrainStartRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      String(payload.attribute ?? ''),
      String(payload.sourceClass ?? payload.source_class ?? ''),
      String(payload.variant ?? 'mobilenetv4'),
      values,
      (hp.epochs ?? 20) | 0,
      (hp.batchSize ?? hp.batch_size ?? 32) | 0,
      Number(hp.learningRate ?? hp.learning_rate ?? 1e-3),
      (hp.imageSize ?? hp.image_size ?? 224) | 0,
      Boolean(hp.freezeBackbone ?? hp.freeze_backbone ?? false),
      String(payload.targetNodeId ?? payload.target_node_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(JobsOverviewRequest) (puste ciało). */
  mlStudioJobsOverviewRequest(correlationId, _payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioJobsOverviewRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(GenericTrainStatusRequest) — polling postępu
   * treningu generycznego (klasyfikator atrybutu i inne tory nie-detekcyjne).
   * Zwraca status + krzywą [{epoch, metricName, value}]. payload: { runId }.
   */
  mlStudioGenericTrainStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioGenericTrainStatusRequest(
      String(payload.runId ?? payload.run_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecogDetectRequest) — detekcja na obrazie
   * wytrenowanym modelem recognition. payload: { modelId, threshold, imageB64 }.
   * Zwraca detectionsJson (lista detekcji) + width/height.
   */
  mlStudioRecogDetectRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogDetectRequest(
      String(payload.modelId ?? payload.model_id ?? ''),
      Number(payload.threshold ?? 0.5),
      String(payload.imageB64 ?? payload.image_b64 ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MlStudioBody(RecogImagesListRequest) — lista obrazów datasetu do galerii
   * anotacji. payload: { datasetId }. Zwraca imagesJson + categoriesJson. */
  mlStudioRecogImagesListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogImagesListRequest(
      String(payload.datasetId ?? payload.dataset_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(RecogImageRequest) — jeden obraz (downscaled b64) + jego bboxy.
   * payload: { datasetId, imageId }. */
  mlStudioRecogImageRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogImageRequest(
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      String(payload.imageId ?? payload.image_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(RecogSaveAnnotationsRequest) — zapis bboxów obrazu do COCO.
   * payload: { datasetId, imageId, annotationsJson, approve }. */
  mlStudioRecogSaveAnnotationsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogSaveAnnotationsRequest(
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      String(payload.imageId ?? payload.image_id ?? ''),
      String(payload.annotationsJson ?? payload.annotations_json ?? '[]'),
      Boolean(payload.approve ?? false),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(SchemaGetRequest) — odczyt schematu rozpoznawania projektu
   * (JSON nieprzezroczysty dla Core). payload: { projectId }. Zwraca schemaJson. */
  mlStudioSchemaGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioSchemaGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(SchemaSaveRequest) — upsert schematu rozpoznawania projektu.
   * Core zapisuje schemaJson dosłownie. payload: { projectId, schemaJson }. */
  mlStudioSchemaSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioSchemaSaveRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.schemaJson ?? payload.schema_json ?? '{}'),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(LookupDictsListRequest) — lista słowników lookup projektu.
   * payload: { projectId }. Zwraca dictsJson (tablica {dictId,name,rowsJson}). */
  mlStudioLookupDictsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioLookupDictsListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(LookupDictSaveRequest) — upsert słownika lookup. Pusty dictId
   * = INSERT (zwraca nowe id). payload: { projectId, dictId, name, rowsJson }. */
  mlStudioLookupDictSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioLookupDictSaveRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.dictId ?? payload.dict_id ?? ''),
      String(payload.name ?? ''),
      String(payload.rowsJson ?? payload.rows_json ?? '[]'),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(LookupDictDeleteRequest) — usuwa słownik lookup po id.
   * payload: { dictId }. */
  mlStudioLookupDictDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioLookupDictDeleteRequest(
      String(payload.dictId ?? payload.dict_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(ServiceModelsListRequest) — modele, do których pole schematu
   * może się przypiąć (service_models + wbudowane in-core). Pusta capability =
   * wszystkie. payload: { capability }. Zwraca modelsJson. */
  mlStudioServiceModelsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioServiceModelsListRequest(
      String(payload.capability ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /**
   * MlStudioBody(VisionModelPublishRequest) — publikuje wytrenowany model do
   * rejestru vision_models (kamery). payload: { modelId, modelName, op,
   * threshold?, alias? }. Zwraca { ok, error }.
   */
  mlStudioVisionModelPublishRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const threshold = payload.threshold;
    const alias = payload.alias;
    const body = _wasm.encodeMlStudioVisionModelPublishRequest(
      String(payload.modelId ?? payload.model_id ?? ''),
      String(payload.modelName ?? payload.model_name ?? ''),
      String(payload.op ?? ''),
      threshold === null || threshold === undefined || threshold === '' ? undefined : Number(threshold),
      alias === null || alias === undefined || alias === '' ? undefined : String(alias),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(VisionModelsListRequest) — lista rejestru modeli wizyjnych
   * (dynamiczne ONNX dla pipeline'ów kamer). Zwraca { models: [...] }. */
  mlStudioVisionModelsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioVisionModelsListRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** VisionImportBody(FetchManifestRequest) — Core pobiera zdalny manifest
   * modeli przez klucz API (deploy wizard "Własny"). payload: { manifestUrl,
   * apiKey }. Zwraca { bundle, files, model, error }. */
  visionImportFetchManifestRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeVisionImportFetchManifestRequest(
      String(payload.manifestUrl ?? payload.manifest_url ?? ''),
      String(payload.apiKey ?? payload.api_key ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** VisionImportBody(ImportRequest) — Core importuje pojedynczy model rejestru
   * ze zdalnej instancji do lokalnego rejestru vision_models. payload:
   * { manifestUrl, apiKey, modelName, alias? }. Zwraca { ok, importedModelName, error }. */
  visionImportModelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const alias = payload.alias;
    const body = _wasm.encodeVisionImportModelRequest(
      String(payload.manifestUrl ?? payload.manifest_url ?? ''),
      String(payload.apiKey ?? payload.api_key ?? ''),
      String(payload.modelName ?? payload.model_name ?? ''),
      alias === null || alias === undefined || alias === '' ? undefined : String(alias),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MlStudioBody(VisionModelDeleteRequest) — usuwa model z rejestru vision.
   * payload: { modelName }. Zwraca { ok, error }. */
  mlStudioVisionModelDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioVisionModelDeleteRequest(
      String(payload.modelName ?? payload.model_name ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /**
   * MessageBody::MlStudioBody(RecogDatasetRegisterRequest) — rejestruje dataset
   * COCO przez ŚCIEŻKĘ do katalogu na serwerze (duże zbiory obrazów ponad limit
   * ramki WS). payload: { projectId, name, path }.
   */
  mlStudioRecogDatasetRegisterRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogDatasetRegisterRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.name ?? ''),
      String(payload.path ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecogStageMediaRequest) — jeden fragment surowego
   * pliku media (obraz/wideo) wgrywanego do staging projektu recognition.
   * payload: { projectId, filename, uploadId, seq, totalChunks, bytes }.
   */
  mlStudioRecogStageMediaRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogStageMediaRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.filename ?? ''),
      String(payload.uploadId ?? payload.upload_id ?? ''),
      Number(payload.seq ?? 0) >>> 0,
      Number(payload.totalChunks ?? payload.total_chunks ?? 0) >>> 0,
      payload.bytes instanceof Uint8Array ? payload.bytes : new Uint8Array(payload.bytes ?? []),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecogBuildDatasetRequest) — buduje dataset COCO z
   * wcześniej wgranych plików staging (kopia obrazów, dekodowanie HEIC, ekstrakcja
   * klatek wideo). payload: { projectId, datasetName, fps, sourceDir? }. Gdy
   * sourceDir jest podane, Core skanuje ten katalog na serwerze rekurencyjnie
   * zamiast staging.
   */
  mlStudioRecogBuildDatasetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const sourceDir = payload.sourceDir ?? payload.source_dir ?? '';
    const body = _wasm.encodeMlStudioRecogBuildDatasetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.datasetName ?? payload.dataset_name ?? ''),
      Number(payload.fps ?? 5) >>> 0,
      sourceDir ? String(sourceDir) : undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecogBuildStatusRequest) — polling postępu
   * asynchronicznej budowy datasetu COCO. payload: { buildId }. Zwraca status
   * (running|succeeded|failed), licznik plików/klatek i dataset po sukcesie.
   */
  mlStudioRecogBuildStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogBuildStatusRequest(
      String(payload.buildId ?? payload.build_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecogAutolabelRequest) — auto-etykietuje cały dataset
   * COCO wbudowanym detektorem RF-DETR (ADR). payload: { datasetId, threshold, mode }
   * (mode: 'only_empty'|'overwrite'). Zwraca jobId do pollingu statusu.
   */
  mlStudioRecogAutolabelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogAutolabelRequest(
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      Number(payload.threshold ?? 0.5),
      String(payload.mode ?? 'only_empty'),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecogAutolabelStatusRequest) — polling postępu
   * asynchronicznego auto-etykietowania. payload: { jobId }. Zwraca status
   * (running|succeeded|failed), licznik obrazów i łączną liczbę detekcji.
   */
  mlStudioRecogAutolabelStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogAutolabelStatusRequest(
      String(payload.jobId ?? payload.job_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(FtExportRequest) — startuje ASYNCHRONICZNY eksport
   * wytrenowanego modelu FT do GGUF. Odpowiedź wraca natychmiast ze statusem
   * 'running'; postęp odpytuj przez mlStudioFtExportStatusRequest.
   * payload: { modelId, outtype } (outtype: 'f16'|'q8_0').
   */
  mlStudioFtExportRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioFtExportRequest(
      String(payload.modelId ?? payload.model_id ?? ''),
      String(payload.outtype ?? 'q8_0'),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(FtExportStatusRequest) — polling postępu eksportu
   * GGUF. Zwraca status + ggufPath + sizeBytes po sukcesie.
   * payload: { modelId }.
   */
  mlStudioFtExportStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioFtExportStatusRequest(
      String(payload.modelId ?? payload.model_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(FtDeployRequest) — DEPLOY wytrenowanego modelu FT
   * (lokalny GGUF po eksporcie) jako embedded serwisu inferencji llama.cpp.
   * Domyka cykl FT: trenuj→eksportuj→DEPLOY→używaj. Odpowiedź zawiera modelName
   * (alias w routingu /v1) + status ('deploying'|'failed').
   * payload: { modelId }.
   */
  mlStudioFtDeployRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioFtDeployRequest(
      String(payload.modelId ?? payload.model_id ?? ''),
      String(payload.targetNodeId ?? payload.target_node_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(FtChatRequest). Zapytanie do wdrożonego modelu FT
   * (test/„użyj"). Gdy model żyje na innym węźle mesh, Core proxuje przez MlChat.
   * payload: { modelId, message, maxTokens? }
   */
  mlStudioFtChatRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioFtChatRequest(
      String(payload.modelId ?? payload.model_id ?? ''),
      String(payload.message ?? ''),
      Number(payload.maxTokens ?? payload.max_tokens ?? 256),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(ProjectExportStartRequest) — starts a background job
   * that packs the whole ML Studio project (datasets, classes, optional models and
   * history) into an archive. Poll progress via mlStudioProjectExportStatusRequest.
   * payload: { projectId, includeModels, includeHistory }.
   */
  mlStudioProjectExportStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectExportStartRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      Boolean(payload.includeModels ?? payload.include_models ?? false),
      Boolean(payload.includeHistory ?? payload.include_history ?? false),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectExportStatusRequest). payload: { jobId }. */
  mlStudioProjectExportStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectExportStatusRequest(
      String(payload.jobId ?? payload.job_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(ProjectImportUploadChunkRequest) — one fragment of a
   * multi-GB project archive. The client splits the file into seq (0..totalChunks)
   * parts under a shared uploadId. payload: { uploadId, seq, totalChunks, filename, bytes }.
   */
  mlStudioProjectImportUploadChunkRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const raw = payload.bytes;
    const bytes = raw instanceof Uint8Array ? raw : new Uint8Array(raw ?? []);
    const body = _wasm.encodeMlStudioProjectImportUploadChunkRequest(
      String(payload.uploadId ?? payload.upload_id ?? ''),
      (payload.seq ?? 0) >>> 0,
      (payload.totalChunks ?? payload.total_chunks ?? 0) >>> 0,
      String(payload.filename ?? ''),
      bytes,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectImportUploadStatusRequest). payload: { uploadId }. */
  mlStudioProjectImportUploadStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectImportUploadStatusRequest(
      String(payload.uploadId ?? payload.upload_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectImportPreviewRequest). payload: { uploadId }. */
  mlStudioProjectImportPreviewRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectImportPreviewRequest(
      String(payload.uploadId ?? payload.upload_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(ProjectImportApplyRequest) — commits an uploaded
   * archive. mode: 'new_project' | 'merge'. The three optional apply fields encode
   * as undefined (None) when absent, NOT empty string.
   * payload: { uploadId, mode, nameOverride?, targetProjectId?, targetDatasetId? }.
   */
  mlStudioProjectImportApplyRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const nameOverride = payload.nameOverride ?? payload.name_override;
    const targetProjectId = payload.targetProjectId ?? payload.target_project_id;
    const targetDatasetId = payload.targetDatasetId ?? payload.target_dataset_id;
    const body = _wasm.encodeMlStudioProjectImportApplyRequest(
      String(payload.uploadId ?? payload.upload_id ?? ''),
      String(payload.mode ?? ''),
      nameOverride != null && nameOverride !== '' ? String(nameOverride) : undefined,
      targetProjectId != null && targetProjectId !== '' ? String(targetProjectId) : undefined,
      targetDatasetId != null && targetDatasetId !== '' ? String(targetDatasetId) : undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectImportStatusRequest). payload: { jobId }. */
  mlStudioProjectImportStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectImportStatusRequest(
      String(payload.jobId ?? payload.job_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(ProjectImportCancelRequest). payload: { uploadId }. */
  mlStudioProjectImportCancelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioProjectImportCancelRequest(
      String(payload.uploadId ?? payload.upload_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecordingsListRequest) — lists recordings filtered by
   * camera and time range (unix ms). Absent filters encode as undefined (None).
   * payload: { cameraId?, dateFromMs?, dateToMs?, limit }.
   */
  mlStudioRecordingsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const cameraId = payload.cameraId ?? payload.camera_id;
    const dateFromMs = payload.dateFromMs ?? payload.date_from_ms;
    const dateToMs = payload.dateToMs ?? payload.date_to_ms;
    const body = _wasm.encodeMlStudioRecordingsListRequest(
      cameraId != null && cameraId !== '' ? String(cameraId) : undefined,
      dateFromMs != null ? Number(dateFromMs) : undefined,
      dateToMs != null ? Number(dateToMs) : undefined,
      (payload.limit ?? 0) >>> 0,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RecogImportRecordingsRequest) — imports recordings into
   * a recognition dataset: extracts frames at fps, optional autolabel. collision:
   * 'suffix' | 'skip'. payload: { projectId, datasetId, recordingRefs, fps, autolabel, collision }.
   */
  mlStudioRecogImportRecordingsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const refs = Array.isArray(payload.recordingRefs ?? payload.recording_refs)
      ? (payload.recordingRefs ?? payload.recording_refs).map((r) => String(r))
      : [];
    const body = _wasm.encodeMlStudioRecogImportRecordingsRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      refs,
      (payload.fps ?? 5) >>> 0,
      Boolean(payload.autolabel ?? false),
      String(payload.collision ?? 'suffix'),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(RecogImportRecordingsStatusRequest). payload: { jobId }. */
  mlStudioRecogImportRecordingsStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRecogImportRecordingsStatusRequest(
      String(payload.jobId ?? payload.job_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::SkillsBody(ListRequest) — UserSession. Skills registry list
   * with optional tag/source/status filters.
   * payload: { tag?, source?, status? }
   */
  skillsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsListRequest(
      payload.tag ?? null,
      payload.source ?? null,
      payload.status ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  skillsDetailRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsDetailRequest(String(payload.skillId ?? payload.skill_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  skillsUpsertRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsUpsertRequest(
      typeof payload.skillJson === 'string' ? payload.skillJson : JSON.stringify(payload.skill ?? payload),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  skillsDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsDeleteRequest(String(payload.skillId ?? payload.skill_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  skillsForkRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsForkRequest(
      String(payload.skillId ?? payload.skill_id ?? ''),
      String(payload.newName ?? payload.new_name ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::SkillsBody(HubSearchRequest) — Admin. Search the configured
   * GitHub taps (or one given source) for importable skills.
   * payload: { query, source? }
   */
  skillsHubSearchRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsHubSearchRequest(
      String(payload.query ?? ''),
      payload.source != null && payload.source !== '' ? String(payload.source) : null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::SkillsBody(HubImportRequest) — Admin. Fetch a skill from a
   * GitHub repo path or a direct SKILL.md URL into quarantine + scan it.
   * payload: { source, gitRef? }
   */
  skillsHubImportRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const ref = payload.gitRef ?? payload.git_ref;
    const body = _wasm.encodeSkillsHubImportRequest(
      String(payload.source ?? ''),
      ref != null && ref !== '' ? String(ref) : null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  skillsHubApproveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsHubApproveRequest(String(payload.skillId ?? payload.skill_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  skillsHubRejectRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsHubRejectRequest(String(payload.skillId ?? payload.skill_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::SkillsBody(CuratorRunRequest) — Admin. Run a curator review pass:
   * the auxiliary model proposes merge/umbrella/archive actions over the skill
   * index (no mutation). Returns { proposalJson, snapshotId }.
   */
  skillsCuratorRunRequest(correlationId, _payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsCuratorRunRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::SkillsBody(CuratorApplyRequest) — Admin. Apply an admin-approved
   * subset of a curator proposal against its snapshot.
   * payload: { snapshotId, approvedActions: number[] }
   */
  skillsCuratorApplyRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const approved = Array.isArray(payload.approvedActions) ? payload.approvedActions : [];
    const body = _wasm.encodeSkillsCuratorApplyRequest(
      String(payload.snapshotId ?? payload.snapshot_id ?? ''),
      JSON.stringify(approved),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::SkillsBody(CuratorRollbackRequest) — Admin. Restore the captured
   * pre-apply rows of an applied snapshot.
   * payload: { snapshotId }
   */
  skillsCuratorRollbackRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSkillsCuratorRollbackRequest(
      String(payload.snapshotId ?? payload.snapshot_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::AgentsBody(ListRequest) — UserSession. Agents registry list
   * with optional enabled/routable filters (booleans or null = no filter).
   * payload: { enabled?, routable? }
   */
  agentsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentsListRequest(
      typeof payload.enabled === 'boolean' ? payload.enabled : null,
      typeof payload.routable === 'boolean' ? payload.routable : null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  agentsDetailRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentsDetailRequest(String(payload.agentId ?? payload.agent_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  agentsUpsertRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentsUpsertRequest(
      typeof payload.agentJson === 'string' ? payload.agentJson : JSON.stringify(payload.agent ?? payload),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  agentsDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentsDeleteRequest(String(payload.agentId ?? payload.agent_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  agentRunsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const agentId = payload.agentId ?? payload.agent_id;
    const parentRunId = payload.parentRunId ?? payload.parent_run_id;
    const body = _wasm.encodeAgentRunsListRequest(
      agentId ? String(agentId) : null,
      payload.status ? String(payload.status) : null,
      parentRunId ? String(parentRunId) : null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  agentRunDetailRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentRunDetailRequest(String(payload.runId ?? payload.run_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  toolsCatalogRequest(correlationId, _payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeToolsCatalogRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::AgentsBody(RunCancelRequest) — UserSession. Cancels one live
   * run (ACL: run principal or admin). payload: { runId }
   */
  agentRunCancelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentRunCancelRequest(String(payload.runId ?? payload.run_id ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::AgentsBody(RunEventsSubscribeRequest) — UserSession. Long-lived
   * stream of AgentRunEvent frames for a scope. payload: { scopeKind:
   * 'session'|'run', scopeId }. Use via ApiBinary.subscribe — the stream stays
   * open until cancel/disconnect.
   */
  agentRunEventsSubscribeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentRunEventsSubscribeRequest(
      String(payload.scopeKind ?? payload.scope_kind ?? 'session'),
      String(payload.scopeId ?? payload.scope_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  syncConflictsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSyncConflictsListRequest(
      String(payload.orgId ?? payload.org_id ?? 'org-default'),
      String(payload.addonId ?? payload.addon_id ?? ''),
      String(payload.status ?? 'open'),
      Number(payload.limit ?? 100) >>> 0,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  syncConflictResolveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSyncConflictResolveRequest(
      String(payload.orgId ?? payload.org_id ?? 'org-default'),
      String(payload.addonId ?? payload.addon_id ?? ''),
      String(payload.operationId ?? payload.operation_id ?? ''),
      String(payload.resolution ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  syncStorageReportRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSyncStorageReportRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::AuditLogExportRequest — Admin. Eksport CSV z filtrami
   * (max 100_000 wierszy). payload: { userId?, addonId?, action?, fromDate?, toDate?, search? }
   */
  auditLogExportRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAuditLogExportRequest(
      payload.userId == null ? null : String(payload.userId),
      payload.addonId ?? null,
      payload.action ?? null,
      payload.fromDate ?? null,
      payload.toDate ?? null,
      payload.search ?? null,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::AuditLogCleanupRequest — Admin. Usuwa wpisy starsze niz
   * `keepDays` dni. payload: { keepDays }
   */
  auditLogCleanupRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAuditLogCleanupRequest(Number(payload.keepDays ?? 90) >>> 0);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // =============================================================================
  // Addon permissions + OAuth (migracja 38)
  // =============================================================================

  /** MessageBody::AddonDetailRequest — szczegoly addona (perms + oauth providers). */
  addonDetailRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonDetailRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonVisibilityListRequest — widocznosc per grupa. */
  addonVisibilityListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonVisibilityListRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonVisibilitySetRequest — ustawia widocznosc per grupa. */
  addonVisibilitySetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonVisibilitySetRequest(
      String(payload.addonId ?? ''),
      String(payload.groupId ?? ''),
      Boolean(payload.visible),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonAdminOnlySetRequest — przelacza admin_only dla addona. */
  addonAdminOnlySetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonAdminOnlySetRequest(
      String(payload.addonId ?? ''),
      Boolean(payload.adminOnly),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonShowInCatalogSetRequest — przelacza show_in_catalog. */
  addonShowInCatalogSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonShowInCatalogSetRequest(
      String(payload.addonId ?? ''),
      Boolean(payload.showInCatalog),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonPermissionCatalogRequest — lista deklaracji uprawnien. */
  addonPermissionCatalogRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonPermissionCatalogRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonPermissionMatrixRequest — aktualna macierz grantow + defaults. */
  addonPermissionMatrixRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonPermissionMatrixRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonPermissionSetRequest — set grant dla (user|group). */
  addonPermissionSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonPermissionSetRequest(
      String(payload.addonId ?? ''),
      String(payload.subjectType ?? 'user'),
      String(payload.subjectId ?? ''),
      String(payload.permissionId ?? ''),
      String(payload.grantMode ?? 'inherit'),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonPermissionDefaultSetRequest — domyslny grant dla addona. */
  addonPermissionDefaultSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonPermissionDefaultSetRequest(
      String(payload.addonId ?? ''),
      String(payload.permissionId ?? ''),
      String(payload.grantMode ?? 'deny'),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonPermissionCheckRequest — sprawdz efektywny grant. */
  addonPermissionCheckRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const userId = payload.userId == null ? null : String(payload.userId);
    const body = _wasm.encodeAddonPermissionCheckRequest(
      String(payload.addonId ?? ''),
      String(payload.permissionId ?? ''),
      userId,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonOAuthConfigListRequest — lista konfiguracji (zero secretow). */
  addonOAuthConfigListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonOAuthConfigListRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonOAuthConfigSetRequest — zapis konfiguracji (secret opcjonalny). */
  addonOAuthConfigSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const secret = payload.clientSecret == null ? null : String(payload.clientSecret);
    const body = _wasm.encodeAddonOAuthConfigSetRequest(
      String(payload.addonId ?? ''),
      String(payload.providerId ?? ''),
      String(payload.clientId ?? ''),
      secret,
      String(payload.redirectUri ?? ''),
      Boolean(payload.enabled),
      String(payload.oauthMode ?? payload.oauth_mode ?? 'individual'),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonOAuthConfigClearSecretRequest — usun wylacznie secret. */
  addonOAuthConfigClearSecretRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonOAuthConfigClearSecretRequest(
      String(payload.addonId ?? ''),
      String(payload.providerId ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonOAuthAuthorizeStartRequest — inicjuje flow autoryzacji. */
  addonOAuthAuthorizeStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const redirectAfter = payload.redirectAfter == null ? null : String(payload.redirectAfter);
    const body = _wasm.encodeAddonOAuthAuthorizeStartRequest(
      String(payload.addonId ?? ''),
      String(payload.providerId ?? ''),
      String(payload.mode ?? 'individual'),
      redirectAfter,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonOAuthLinkedAccountsRequest — lista polaczonych kont. */
  addonOAuthLinkedAccountsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonOAuthLinkedAccountsRequest(
      String(payload.addonId ?? ''),
      String(payload.scope ?? 'mine'),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonOAuthRevokeRequest — unieważnij konto. */
  addonOAuthRevokeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonOAuthRevokeRequest(Number(payload.accountId ?? 0));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonOAuthReauthorizeRequest — nowy flow dla istniejacego konta. */
  addonOAuthReauthorizeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonOAuthReauthorizeRequest(Number(payload.accountId ?? 0));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonOAuthTestConnectionRequest — admin probes provider. */
  addonOAuthTestConnectionRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonOAuthTestConnectionRequest(
      String(payload.addonId ?? payload.addon_id ?? ''),
      String(payload.providerId ?? payload.provider_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MyOAuthAccountsListRequest (unit) — konta biezacego usera. */
  myOAuthAccountsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMyOAuthAccountsListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // =============================================================================
  // Addon lifecycle (toggle/install/uninstall/config/logs/tools/resources/network/reload)
  // =============================================================================

  /** MessageBody::AddonToggleRequest — wlacza/wylacza addon. */
  addonToggleRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonToggleRequest(
      String(payload.addonId ?? ''),
      Boolean(payload.enabled),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::DeployVllmRecommendRequest — vLLM config recommend (CBOR passthrough). */
  deployVllmRecommendRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeDeployVllmRecommendRequest(JSON.stringify(payload));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::SuggestServicePortRequest — first free host port for the deploy form. */
  suggestServicePortRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeSuggestServicePortRequest(JSON.stringify(payload));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonInstallRequest — instaluje addon z ZIP (Uint8Array content). */
  addonInstallRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const content = payload.content instanceof Uint8Array
      ? payload.content
      : new Uint8Array(payload.content ?? []);
    const body = _wasm.encodeAddonInstallRequest(
      String(payload.filename ?? 'addon.zip'),
      content,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonUninstallRequest — odinstalowuje addon. */
  addonUninstallRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonUninstallRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonConfigGetRequest — schema + values (secret pola puste). */
  addonConfigGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonConfigGetRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonConfigSetRequest — zapisuje wartosci konfiguracji. */
  addonConfigSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    // payload.values = { key: value } lub tablica [[k,v],...] — normalizujemy.
    const entries = Array.isArray(payload.values)
      ? payload.values
      : Object.entries(payload.values ?? {});
    const keys = entries.map((e) => String(e[0]));
    const vals = entries.map((e) => String(e[1]));
    const body = _wasm.encodeAddonConfigSetRequest(
      String(payload.addonId ?? ''),
      keys,
      vals,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonLogsRequest — per-addon wpisy audytu z paginacja. */
  addonLogsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonLogsRequest(
      String(payload.addonId ?? ''),
      Number(payload.limit ?? 50),
      Number(payload.offset ?? 0),
      payload.level == null ? undefined : String(payload.level),
      payload.search == null ? undefined : String(payload.search),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonToolsRequest — deklaracje narzedzi z manifestu. */
  addonToolsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonToolsRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonResourcesGetRequest — pobiera limity zasobow. */
  addonResourcesGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonResourcesGetRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonResourcesSetRequest — zapisuje limity zasobow. */
  addonResourcesSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonResourcesSetRequest(
      String(payload.addonId ?? ''),
      Number(payload.maxInstances ?? 0),
      Number(payload.cpuLimitPct ?? 0),
      Number(payload.ramMb ?? 0),
      Number(payload.storageMb ?? 0),
      Number(payload.httpRequestsPerMin ?? 0),
      Number(payload.llmTokensPerMin ?? 0),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonNetworkRulesGetRequest — allowed/blocked + mode. */
  addonNetworkRulesGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonNetworkRulesGetRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonNetworkRulesSetRequest — zapisuje listy hostow + mode. */
  addonNetworkRulesSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const allowed = Array.isArray(payload.allowedHosts)
      ? payload.allowedHosts.map((h) => String(h))
      : [];
    const blocked = Array.isArray(payload.blockedHosts)
      ? payload.blockedHosts.map((h) => String(h))
      : [];
    const body = _wasm.encodeAddonNetworkRulesSetRequest(
      String(payload.addonId ?? ''),
      allowed,
      blocked,
      String(payload.mode ?? 'strict'),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::AddonReloadRequest — re-inicjalizuje instance pool addona. */
  addonReloadRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAddonReloadRequest(String(payload.addonId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // ==== IAM (users + groups + permissions) ====
  iamListUsersRequest(correlationId, _payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamListUsersRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamGetUserRequest(correlationId, { userId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamGetUserRequest(String(userId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamCreateUserRequest(correlationId, p, sequence = 1) {
    assertReady();
    const csv = Array.isArray(p.groupIds) ? p.groupIds.join(',') : String(p.groupIds ?? '');
    const body = _wasm.encodeIamCreateUserRequest(
      String(p.username ?? ''), String(p.password ?? ''), String(p.displayName ?? ''),
      String(p.email ?? ''), String(p.role ?? 'user'), csv,
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamUpdateUserRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamUpdateUserRequest(
      String(p.userId), String(p.displayName ?? ''), String(p.email ?? ''),
      !!p.isActive, String(p.role ?? 'user'),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamDeleteUserRequest(correlationId, { userId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamDeleteUserRequest(String(userId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamSetUserGroupsRequest(correlationId, p, sequence = 1) {
    assertReady();
    const csv = Array.isArray(p.groupIds) ? p.groupIds.join(',') : String(p.groupIds ?? '');
    const body = _wasm.encodeIamSetUserGroupsRequest(String(p.userId), csv);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamResetUserPasswordRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamResetUserPasswordRequest(String(p.userId), String(p.newPassword ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamListGroupsRequest(correlationId, _payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamListGroupsRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamCreateGroupRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamCreateGroupRequest(String(p.name ?? ''), String(p.description ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamUpdateGroupRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamUpdateGroupRequest(String(p.groupId), String(p.name ?? ''), String(p.description ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamDeleteGroupRequest(correlationId, { groupId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamDeleteGroupRequest(String(groupId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamGroupMembersRequest(correlationId, { groupId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamGroupMembersRequest(String(groupId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamSetPermissionRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamSetPermissionRequest(
      String(p.resourceType), String(p.resourceId),
      String(p.subjectType), String(p.subjectId), String(p.accessLevel),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamClearPermissionRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamClearPermissionRequest(
      String(p.resourceType), String(p.resourceId),
      String(p.subjectType), String(p.subjectId),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamListPermsForResourceRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamListPermsForResourceRequest(String(p.resourceType), String(p.resourceId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamListPermsForSubjectRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamListPermsForSubjectRequest(String(p.subjectType), String(p.subjectId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  // ---- Services (Krok N2 — packed in MessageBody::ServiceBody) -----------

  /**
   * MessageBody::ServiceBody(ServicePayload::ReqList). The list shape lets the
   * shared `ApiBinary.list('serviceListRequest', …)` helper call without a
   * payload (it forwards `(corrId, sequence)`); we accept both call styles.
   */
  serviceListRequest(correlationId, payloadOrSeq, sequence = 1) {
    assertReady();
    let payload = {};
    let seq = sequence;
    if (typeof payloadOrSeq === 'number' || typeof payloadOrSeq === 'bigint') {
      seq = payloadOrSeq;
    } else if (payloadOrSeq && typeof payloadOrSeq === 'object') {
      payload = payloadOrSeq;
    }
    const engineFilter = payload.engineIdFilter ? String(payload.engineIdFilter) : undefined;
    const categoryFilter = payload.categoryFilter ? String(payload.categoryFilter) : undefined;
    const body = _wasm.encodeServiceListRequest(engineFilter, categoryFilter);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(seq),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqDelete). */
  serviceDeleteRequest(correlationId, { serviceId, nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceDeleteRequest(Number(serviceId), nodeId ?? undefined);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqPin). */
  servicePinRequest(correlationId, { serviceId, pinned, nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServicePinRequest(
      Number(serviceId),
      Boolean(pinned),
      nodeId ?? undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqPause). */
  servicePauseRequest(correlationId, { serviceId, paused, nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServicePauseRequest(
      Number(serviceId),
      Boolean(paused),
      nodeId ?? undefined,
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::ServiceBody(ServicePayload::ReqStart) — unpause + spawn. */
  serviceStartRequest(correlationId, { serviceId, nodeId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceStartRequest(Number(serviceId), nodeId ?? undefined);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // ---- Legal documents (RODO/GDPR admin, F2-P8.d M10) -------------------

  /**
   * MessageBody::LegalAdminBody(ListRequest) — admin list of generated RODO
   * documents. `includeRevoked=false` hides soft-deleted rows.
   */
  legalDocumentsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeLegalDocumentsListRequest(Boolean(payload.includeRevoked));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::LegalAdminBody(GenerateRequest) — render and persist a new
   * RODO PDF. `variant` must be one of `short` | `standard` | `full`.
   */
  legalDocumentGenerateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeLegalDocumentGenerateRequest(String(payload.variant ?? 'standard'));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::LegalAdminBody(RevokeRequest) — soft-delete a legal document
   * by its UUID.
   */
  legalDocumentRevokeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeLegalDocumentRevokeRequest(String(payload.docId ?? ''));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::StreamBody(SubscribeRequest) — subskrypcja strumienia
   *  zarejestrowanego w StreamHub (Chunk B). Server odpowiada
   *  SubscribeResponse + sekwencja Frame chunkow + Closed na tym samym
   *  correlation_id. Payload: { streamId, preview? } — preview=true wybiera
   *  wariant podgladu 720p/~1,5 Mbit/s dla strumieni camera: (kafelki Live
   *  view), domyslnie pelna jakosc. */
  streamSubscribeRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const streamId = String(payload?.streamId ?? payload?.stream_id ?? '');
    const preview = Boolean(payload?.preview ?? false);
    const body = _wasm.encodeStreamSubscribeRequest(streamId, preview);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::StreamBody(CloseRequest) — wczesna rezygnacja z aktywnej
   *  subskrypcji. Wysylane na tym samym correlation_id co oryginalny
   *  SubscribeRequest. Payload: { streamId }. */
  streamCloseRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const streamId = String(payload?.streamId ?? payload?.stream_id ?? '');
    const body = _wasm.encodeStreamCloseRequest(streamId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  // ---------------------------------------------------------------------------
  // RoleCatalogBody — katalog ról biznesowych (admin). Payload przekazywany do
  // WASM jako JSON string aby wesprzec Vec<(String,String)> + Option<Option<_>>
  // bez serde-wasm-bindgen w tym crate'cie.
  // ---------------------------------------------------------------------------

  /** MessageBody::RoleCatalogBody(ListRequest). Payload: { kind?, isActive?, search?, limit?, offset? }. */
  roleCatalogListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const filter = {
      kind: payload?.kind ?? null,
      is_active: payload?.isActive ?? payload?.is_active ?? null,
      search: payload?.search ?? null,
      limit: payload?.limit ?? null,
      offset: payload?.offset ?? null,
    };
    const body = _wasm.encodeRoleCatalogListRequest(JSON.stringify(filter));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::RoleCatalogBody(GetRequest). Payload: { id }. */
  roleCatalogGetRequest(correlationId, { id }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRoleCatalogGetRequest(String(id));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::RoleCatalogBody(GetBySlugRequest). Payload: { slug }. */
  roleCatalogGetBySlugRequest(correlationId, { slug }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRoleCatalogGetBySlugRequest(String(slug));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::RoleCatalogBody(ListLocalesRequest) — unit. */
  roleCatalogListLocalesRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRoleCatalogListLocalesRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::RoleCatalogBody(CreateRequest). Payload (camelCase z UI) jest
   *  remapowany na snake_case zgodny z DTO `RoleCatalogCreateRequest`. */
  roleCatalogCreateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const req = {
      slug: String(payload?.slug ?? ''),
      kind: String(payload?.kind ?? ''),
      name_translations: payload?.nameTranslations ?? payload?.name_translations ?? [],
      description_translations:
        payload?.descriptionTranslations ?? payload?.description_translations ?? [],
      icon: payload?.icon ?? null,
      color_hint: payload?.colorHint ?? payload?.color_hint ?? null,
      is_manager: !!(payload?.isManager ?? payload?.is_manager ?? false),
      default_visibility_scope: String(
        payload?.defaultVisibilityScope ?? payload?.default_visibility_scope ?? 'assigned',
      ),
    };
    const body = _wasm.encodeRoleCatalogCreateRequest(JSON.stringify(req));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::RoleCatalogBody(UpdateRequest) — patch.
   *  Pola w `payload` mogą być nieobecne (nie ruszaj), `null` (wyczysc) lub
   *  konkretną wartością. icon/colorHint mapowane na Option<Option<String>>. */
  roleCatalogUpdateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const req = { id: String(payload?.id ?? '') };
    if (Object.prototype.hasOwnProperty.call(payload, 'kind') && payload.kind !== undefined) {
      req.kind = payload.kind === null ? null : String(payload.kind);
    }
    if (
      Object.prototype.hasOwnProperty.call(payload, 'nameTranslations')
      && payload.nameTranslations !== undefined
    ) {
      req.name_translations = payload.nameTranslations;
    }
    if (
      Object.prototype.hasOwnProperty.call(payload, 'descriptionTranslations')
      && payload.descriptionTranslations !== undefined
    ) {
      req.description_translations = payload.descriptionTranslations;
    }
    // icon: Option<Option<String>> — w JSON: brak pola = None, null = Some(None) (clear),
    // string = Some(Some(value)).
    if (Object.prototype.hasOwnProperty.call(payload, 'icon') && payload.icon !== undefined) {
      req.icon = payload.icon === null ? null : String(payload.icon);
    }
    if (Object.prototype.hasOwnProperty.call(payload, 'colorHint') && payload.colorHint !== undefined) {
      req.color_hint = payload.colorHint === null ? null : String(payload.colorHint);
    }
    if (Object.prototype.hasOwnProperty.call(payload, 'isManager') && payload.isManager !== undefined) {
      req.is_manager = !!payload.isManager;
    }
    if (
      Object.prototype.hasOwnProperty.call(payload, 'defaultVisibilityScope')
      && payload.defaultVisibilityScope !== undefined
    ) {
      req.default_visibility_scope = String(payload.defaultVisibilityScope);
    }
    const body = _wasm.encodeRoleCatalogUpdateRequest(JSON.stringify(req));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::RoleCatalogBody(DeactivateRequest). Payload: { id }. */
  roleCatalogDeactivateRequest(correlationId, { id }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeRoleCatalogDeactivateRequest(String(id));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

};

// =============================================================================
// Decode helpery
// =============================================================================

/**
 * Dekoduje binary WebSocket frame.
 * Zwraca `{envelope, body}` gdzie:
 *  - envelope: widok primitives (correlation_id BigInt, sequence u32, flags, ...)
 *  - body: plain JS object z pole `variant` i polami wariantu
 *
 * Rzuca Error na malformed frame — call site powinien logowac i disconnectowac.
 */
export function decodeFrame(bytes) {
  assertReady();
  const view = _wasm.decodeEnvelope(bytes);
  const body = _wasm.decodeMessageBody(view.body);
  return {
    envelope: {
      schemaVersion: view.schema_version,
      correlationId: view.correlation_id,
      sequence: view.sequence,
      messageKind: view.message_kind,
      flags: view.flags,
      isForward: view.is_forward,
      targetNodeId: view.targetNodeId,
      isError: view.isError,
      isStreamChunk: view.isStreamChunk,
      isStreamEnd: view.isStreamEnd,
    },
    body,
  };
}

/**
 * Szybka walidacja frame bez deserializacji body (early reject malformed input).
 */
export function validateFrame(bytes) {
  assertReady();
  return _wasm.validateFrame(bytes);
}

/**
 * Dekoduje SUROWE kanoniczne bajty klatki LiDAR niesione w `StreamFrame.data`
 * (strumień PUSH `streamId = "lidar:<robotId>"`) do projekcji JS:
 * `{ hasFrame, frameSeq, pointCount, layout, resolution, origin, timestampUs,
 * raw, points }`. Layout 36-bajtowego nagłówka pochodzi z sdk-spec (jedno źródło
 * prawdy) — JS nie powiela parsowania. Zniekształcona/za krótka klatka zwraca
 * `{ hasFrame: false }`.
 */
export function decodeLidarFrame(bytes) {
  assertReady();
  return _wasm.decodeLidarFrame(bytes);
}

// =============================================================================
// Helpers
// =============================================================================

/**
 * Konwersja kluczy camelCase → snake_case dla JSON-encoded payload'ów
 * które backend parsuje przez serde (struktury z snake_case fields).
 * Zachowuje wartości bez zmian, mapuje tylko klucze pierwszego poziomu
 * obiektu (rekursja niepotrzebna dla naszych payloadów które są płaskie).
 */
function camelToSnakePayload(obj) {
  // i64 fields (e.g. service_id) decode to JS BigInt, which JSON.stringify
  // cannot serialize. Coerce to Number — these ids are well within the safe
  // integer range, and the wasm decoder parses them back into i64.
  if (typeof obj === 'bigint') return Number(obj);
  if (obj == null || typeof obj !== 'object') return obj;
  if (Array.isArray(obj)) return obj.map(camelToSnakePayload);
  const out = {};
  for (const [k, v] of Object.entries(obj)) {
    const sk = k.replace(/[A-Z]/g, (c) => '_' + c.toLowerCase());
    out[sk] = camelToSnakePayload(v);
  }
  return out;
}

function assertReady() {
  if (!_wasm) {
    throw new Error('codec not ready — await codecReady before calling codec functions');
  }
}

/**
 * Generator monotonicznych correlation_id dla pojedynczego connectiona.
 * Rozpoczyna od losowej wartosci zeby odroznic reconnecty w logach serwera.
 */
export function makeCorrelationIdGenerator(start = null) {
  let next = start !== null ? BigInt(start) : BigInt(Math.floor(Math.random() * 0xffff)) << 32n;
  return () => {
    const value = next;
    next = next + 1n;
    return value;
  };
}
