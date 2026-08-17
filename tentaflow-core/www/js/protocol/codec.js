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
      payload.interfaceName ?? null,
      payload.interfaceIp ?? null,
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

  /** MessageBody::ServiceBody(ServicePayload::ReqAgent) */
  serviceAgentRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceAgentRequest(JSON.stringify(camelToSnakePayload(payload)));
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
   * MessageBody::MlStudioBody(OcrTrainStartRequest) — startuje ASYNCHRONICZNY
   * trening CZYTNIKA OCR (CRNN + CTC) na wierszach wycinków (np. atrybut "kod"
   * klasy tablica_adr, wartości w formacie <kemler>/<UN>). Wycinki, podział na
   * wiersze i etykiety buduje serwis Python. Odpowiedź natychmiast z runId;
   * postęp odpytuj przez mlStudioGenericTrainStatusRequest.
   * payload: { projectId, datasetId, attribute, sourceClass,
   * hyperparams:{epochs,batchSize,learningRate,syntheticPerEpoch,realRepeat},
   * targetNodeId }.
   */
  mlStudioOcrTrainStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const hp = payload.hyperparams ?? {};
    const body = _wasm.encodeMlStudioOcrTrainStartRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      String(payload.attribute ?? ''),
      String(payload.sourceClass ?? payload.source_class ?? ''),
      (hp.epochs ?? 30) | 0,
      (hp.batchSize ?? hp.batch_size ?? 64) | 0,
      Number(hp.learningRate ?? hp.learning_rate ?? 3e-4),
      (hp.syntheticPerEpoch ?? hp.synthetic_per_epoch ?? 20000) | 0,
      (hp.realRepeat ?? hp.real_repeat ?? 8) | 0,
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
   * MessageBody::MlStudioBody(TrainCancelRequest) — anuluje TRWAJĄCY trening
   * (dowolny tor). payload: { runId }.
   */
  mlStudioTrainCancelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioTrainCancelRequest(
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
   * `sourceNodeId` names a paired node to list from; absent = local node.
   * payload: { cameraId?, dateFromMs?, dateToMs?, limit, sourceNodeId? }.
   */
  mlStudioRecordingsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const cameraId = payload.cameraId ?? payload.camera_id;
    const dateFromMs = payload.dateFromMs ?? payload.date_from_ms;
    const dateToMs = payload.dateToMs ?? payload.date_to_ms;
    const sourceNodeId = payload.sourceNodeId ?? payload.source_node_id;
    const body = _wasm.encodeMlStudioRecordingsListRequest(
      cameraId != null && cameraId !== '' ? String(cameraId) : undefined,
      dateFromMs != null ? Number(dateFromMs) : undefined,
      dateToMs != null ? Number(dateToMs) : undefined,
      (payload.limit ?? 0) >>> 0,
      sourceNodeId != null && sourceNodeId !== '' ? String(sourceNodeId) : undefined,
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
   * 'suffix' | 'skip'. `sourceNodeId` pulls the clips from a paired node; absent =
   * local node. payload: { projectId, datasetId, recordingRefs, fps, autolabel,
   * collision, sourceNodeId? }.
   */
  mlStudioRecogImportRecordingsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const refs = Array.isArray(payload.recordingRefs ?? payload.recording_refs)
      ? (payload.recordingRefs ?? payload.recording_refs).map((r) => String(r))
      : [];
    const sourceNodeId = payload.sourceNodeId ?? payload.source_node_id;
    const body = _wasm.encodeMlStudioRecogImportRecordingsRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.datasetId ?? payload.dataset_id ?? ''),
      refs,
      (payload.fps ?? 5) >>> 0,
      Boolean(payload.autolabel ?? false),
      String(payload.collision ?? 'suffix'),
      sourceNodeId != null && sourceNodeId !== '' ? String(sourceNodeId) : undefined,
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
   * MessageBody::MlStudioBody(RemoteImportPreviewRequest) — fetches ONLY the remote
   * share manifest from an unpaired instance for a cheap preview (no archive
   * download). payload: { url, apiKey }.
   */
  mlStudioRemoteImportPreviewRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRemoteImportPreviewRequest(
      String(payload.url ?? ''),
      String(payload.apiKey ?? payload.api_key ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::MlStudioBody(RemoteImportStartRequest) — downloads the remote
   * archive and imports it as a NEW local project. payload: { url, apiKey, nameOverride? }.
   */
  mlStudioRemoteImportStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const nameOverride = payload.nameOverride ?? payload.name_override;
    const body = _wasm.encodeMlStudioRemoteImportStartRequest(
      String(payload.url ?? ''),
      String(payload.apiKey ?? payload.api_key ?? ''),
      nameOverride == null ? undefined : String(nameOverride),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::MlStudioBody(RemoteImportStatusRequest). payload: { jobId }. */
  mlStudioRemoteImportStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeMlStudioRemoteImportStatusRequest(
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
   * MessageBody::AgentsBody(RunStartRequest) — UserSession. Starts a playground
   * run for one agent with a free-form prompt. Response:
   * AgentRunStartResponse { runId }. payload: { agentId, prompt }
   */
  agentRunStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentRunStartRequest(
      String(payload.agentId ?? payload.agent_id ?? ''),
      String(payload.prompt ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::AgentsBody(BuilderAssistRequest) — Admin. One turn of the
   * agent-builder assistant. `messages` is the full chat transcript
   * [{role:'user'|'assistant', content}]. Response: AgentBuilderAssistResponse
   * { resultJson } where resultJson = {"reply", "proposal"|null}.
   * payload: { messagesJson } or { messages }
   */
  agentBuilderAssistRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const messagesJson = typeof payload.messagesJson === 'string'
      ? payload.messagesJson
      : JSON.stringify(payload.messages ?? []);
    const body = _wasm.encodeAgentBuilderAssistRequest(messagesJson);
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

  /**
   * MessageBody::AgentsBody(RunReplyRequest) — UserSession. Odpowiedź operatora
   * na pytanie agenta (core.ask_user). payload: { runId, questionId, answer }
   */
  agentRunReplyRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentRunReplyRequest(
      String(payload.runId ?? payload.run_id ?? ''),
      String(payload.questionId ?? payload.question_id ?? ''),
      String(payload.answer ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /**
   * MessageBody::AgentsBody(PermissionReplyRequest) — UserSession. Decyzja
   * operatora o zgodzie na narzędzie. payload: { runId, requestId, decision }
   */
  agentPermissionReplyRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAgentPermissionReplyRequest(
      String(payload.runId ?? payload.run_id ?? ''),
      String(payload.requestId ?? payload.request_id ?? ''),
      String(payload.decision ?? ''),
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

  // ---------------------------------------------------------------------------
  // ProjectStudioBody — "Projekty" module (registry, members, creator grants,
  // knowledge sources + chunked upload + ingest, KB search, overview/activity,
  // per-user chats, settings/tags). Requests carrying arrays of structs pass a
  // JSON string to WASM (serde parse, same policy as benchmarkSaveRequest);
  // ActivityList.beforeId crosses as a decimal string (i64 exceeds JS Number).
  // IngestStream/ChatStream go through ApiBinary.subscribe, not one().
  // ---------------------------------------------------------------------------

  /** MessageBody::ProjectStudioBody(ProjectsListRequest). payload: { includeArchived? }. */
  projectStudioProjectsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioProjectsListRequest(
      !!(payload.includeArchived ?? payload.include_archived ?? false),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectCreateRequest). payload: { name, description, template, modules:[], members:[{userId,role}] }. */
  projectStudioProjectCreateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const modules = Array.isArray(payload.modules) ? payload.modules.map(String) : [];
    const members = (Array.isArray(payload.members) ? payload.members : []).map((m) => ({
      user_id: String(m.userId ?? m.user_id ?? ''),
      role: String(m.role ?? ''),
    }));
    const body = _wasm.encodeProjectStudioProjectCreateRequest(
      String(payload.name ?? ''),
      String(payload.description ?? ''),
      String(payload.template ?? ''),
      JSON.stringify(modules),
      JSON.stringify(members),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectGetRequest). payload: { projectId }. */
  projectStudioProjectGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioProjectGetRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectUpdateRequest). payload: { projectId, name, description }. */
  projectStudioProjectUpdateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioProjectUpdateRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.name ?? ''),
      String(payload.description ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectArchiveRequest). payload: { projectId, archived }. */
  projectStudioProjectArchiveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioProjectArchiveRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      !!payload.archived,
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectDeleteRequest). payload: { projectId }. */
  projectStudioProjectDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioProjectDeleteRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MembersListRequest). payload: { projectId }. */
  projectStudioMembersListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioMembersListRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MemberCandidatesRequest). payload: { projectId?, query, limit } — projectId omitted = creation wizard. */
  projectStudioMemberCandidatesRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const projectId = payload.projectId ?? payload.project_id;
    const body = _wasm.encodeProjectStudioMemberCandidatesRequest(
      projectId == null || projectId === '' ? undefined : String(projectId),
      String(payload.query ?? ''),
      Number(payload.limit ?? 20),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MembersAddRequest). payload: { projectId, members:[{userId,role}] }. */
  projectStudioMembersAddRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const members = (Array.isArray(payload.members) ? payload.members : []).map((m) => ({
      user_id: String(m.userId ?? m.user_id ?? ''),
      role: String(m.role ?? ''),
    }));
    const body = _wasm.encodeProjectStudioMembersAddRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      JSON.stringify(members),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MemberRoleSetRequest). payload: { projectId, userId, role }. */
  projectStudioMemberRoleSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioMemberRoleSetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.userId ?? payload.user_id ?? ''),
      String(payload.role ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MemberRemoveRequest). payload: { projectId, userId }. */
  projectStudioMemberRemoveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioMemberRemoveRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.userId ?? payload.user_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(OwnershipTransferRequest). payload: { projectId, newOwnerUserId }. */
  projectStudioOwnershipTransferRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioOwnershipTransferRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.newOwnerUserId ?? payload.new_owner_user_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CreatorGrantsListRequest) — admin only. */
  projectStudioCreatorGrantsListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCreatorGrantsListRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CreatorGrantSetRequest) — admin only. payload: { userId, granted }. */
  projectStudioCreatorGrantSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCreatorGrantSetRequest(
      String(payload.userId ?? payload.user_id ?? ''),
      !!payload.granted,
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourcesListRequest). payload: { projectId }. */
  projectStudioSourcesListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSourcesListRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceUploadChunkRequest). payload: { projectId, uploadId, filename, mime, seq, totalChunks, bytes: Uint8Array }. */
  projectStudioSourceUploadChunkRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const raw = payload.bytes;
    const bytes = raw instanceof Uint8Array ? raw : new Uint8Array(raw ?? []);
    const body = _wasm.encodeProjectStudioSourceUploadChunkRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.uploadId ?? payload.upload_id ?? ''),
      String(payload.filename ?? ''),
      String(payload.mime ?? ''),
      Number(payload.seq ?? 0),
      Number(payload.totalChunks ?? payload.total_chunks ?? 0),
      bytes,
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceCreateRequest). payload: { projectId, kind, name, configJson, fileRefs:[] } — starts the ingest job. */
  projectStudioSourceCreateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const fileRefs = Array.isArray(payload.fileRefs ?? payload.file_refs)
      ? (payload.fileRefs ?? payload.file_refs).map(String)
      : [];
    const body = _wasm.encodeProjectStudioSourceCreateRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.kind ?? ''),
      String(payload.name ?? ''),
      String(payload.configJson ?? payload.config_json ?? '{}'),
      JSON.stringify(fileRefs),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceUpdateRequest). payload: { projectId, sourceId, name, configJson }. */
  projectStudioSourceUpdateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSourceUpdateRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.sourceId ?? payload.source_id ?? ''),
      String(payload.name ?? ''),
      String(payload.configJson ?? payload.config_json ?? '{}'),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceDeleteRequest). payload: { projectId, sourceId }. */
  projectStudioSourceDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSourceDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.sourceId ?? payload.source_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceReingestRequest). payload: { projectId, sourceId, fileId? } — fileId = re-ingest one file. */
  projectStudioSourceReingestRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const fileId = payload.fileId ?? payload.file_id;
    const body = _wasm.encodeProjectStudioSourceReingestRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.sourceId ?? payload.source_id ?? ''),
      fileId == null || fileId === '' ? undefined : String(fileId),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(IngestCancelRequest). payload: { projectId, jobId }. */
  projectStudioIngestCancelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioIngestCancelRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.jobId ?? payload.job_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(IngestStatusRequest) — poll 2-4 s, source of truth. payload: { projectId, jobId }. */
  projectStudioIngestStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioIngestStatusRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.jobId ?? payload.job_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceFilesListRequest). payload: { projectId, sourceId, offset, limit, filter? }. */
  projectStudioSourceFilesListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSourceFilesListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.sourceId ?? payload.source_id ?? ''),
      Number(payload.offset ?? 0),
      Number(payload.limit ?? 50),
      String(payload.filter ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceFileDeleteRequest). payload: { projectId, fileId }. */
  projectStudioSourceFileDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSourceFileDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.fileId ?? payload.file_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceFilePreviewRequest) — text-only, server clamps maxBytes. payload: { projectId, fileId, maxBytes }. */
  projectStudioSourceFilePreviewRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSourceFilePreviewRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.fileId ?? payload.file_id ?? ''),
      Number(payload.maxBytes ?? payload.max_bytes ?? 262144),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(KbSearchRequest). payload: { projectId, query, sourceIds:[], limit } — [] = all sources. */
  projectStudioKbSearchRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const sourceIds = Array.isArray(payload.sourceIds ?? payload.source_ids)
      ? (payload.sourceIds ?? payload.source_ids).map(String)
      : [];
    const body = _wasm.encodeProjectStudioKbSearchRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.query ?? ''),
      JSON.stringify(sourceIds),
      Number(payload.limit ?? 10),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(OverviewRequest) — KPIs + recent activity. payload: { projectId }. */
  projectStudioOverviewRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioOverviewRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ActivityListRequest). payload: { projectId, beforeId?, limit } — beforeId sent as decimal string (i64). */
  projectStudioActivityListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const beforeId = payload.beforeId ?? payload.before_id;
    const body = _wasm.encodeProjectStudioActivityListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      beforeId == null || beforeId === '' ? undefined : String(beforeId),
      Number(payload.limit ?? 50),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ChatsListRequest) — caller's own chats only. payload: { projectId }. */
  projectStudioChatsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioChatsListRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ChatCreateRequest). payload: { projectId, title }. */
  projectStudioChatCreateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioChatCreateRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.title ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ChatRenameRequest). payload: { projectId, chatId, title }. */
  projectStudioChatRenameRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioChatRenameRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.chatId ?? payload.chat_id ?? ''),
      String(payload.title ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ChatDeleteRequest). payload: { projectId, chatId }. */
  projectStudioChatDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioChatDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.chatId ?? payload.chat_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ChatHistoryRequest) — paged, newest first. payload: { projectId, chatId, beforeMessageId?, limit }. */
  projectStudioChatHistoryRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const beforeMessageId = payload.beforeMessageId ?? payload.before_message_id;
    const body = _wasm.encodeProjectStudioChatHistoryRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.chatId ?? payload.chat_id ?? ''),
      beforeMessageId == null || beforeMessageId === '' ? undefined : String(beforeMessageId),
      Number(payload.limit ?? 50),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SettingsGetRequest). payload: { projectId }. */
  projectStudioSettingsGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSettingsGetRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SettingsSaveRequest) — partial save, absent fields untouched. payload: { projectId, name?, description?, agents?|agentsJson?, modules? }. `modules` replaces the enabled module set. */
  projectStudioSettingsSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    let agentsJson;
    if (typeof payload.agentsJson === 'string') {
      agentsJson = payload.agentsJson;
    } else if (typeof payload.agents_json === 'string') {
      agentsJson = payload.agents_json;
    } else if (Array.isArray(payload.agents)) {
      agentsJson = JSON.stringify(payload.agents);
    }
    const modulesJson = Array.isArray(payload.modules) ? JSON.stringify(payload.modules.map(String)) : undefined;
    const body = _wasm.encodeProjectStudioSettingsSaveRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      payload.name == null ? undefined : String(payload.name),
      payload.description == null ? undefined : String(payload.description),
      agentsJson,
      modulesJson,
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TagSaveRequest). payload: { projectId, tagId?, name } — tagId omitted = create. */
  projectStudioTagSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const tagId = payload.tagId ?? payload.tag_id;
    const body = _wasm.encodeProjectStudioTagSaveRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      tagId == null || tagId === '' ? undefined : String(tagId),
      String(payload.name ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TagDeleteRequest). payload: { projectId, tagId }. */
  projectStudioTagDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioTagDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.tagId ?? payload.tag_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** Subscribe — live ingest logs/progress. payload: { projectId, jobId }. Chunks = ProjectStudioIngestStreamChunk, end = ProjectStudioIngestStreamEnd. */
  projectStudioIngestStreamRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioIngestStreamRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.jobId ?? payload.job_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** Subscribe — send one user message, stream the assistant reply. payload: { projectId, chatId, message }. Chunks = ProjectStudioChatStreamChunk, end = ProjectStudioChatStreamEnd. */
  projectStudioChatStreamRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioChatStreamRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.chatId ?? payload.chat_id ?? ''),
      String(payload.message ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  // --- Project Studio F2: manual test cases, suites, runs, tasks/defects,
  // agent generation, notifications, reports. Multi-field mutating requests
  // (CaseSave, RunCreate, TaskSave, ...) serialize the whole snake_case field
  // set as ONE JSON string parsed by serde in WASM; simple requests keep
  // explicit parameters like the F1 section above. ---

  /** MessageBody::ProjectStudioBody(CasesListRequest). payload: { projectId, kind?, status?, priority?, tagId?, origin?, search?, offset?, limit? } — empty filters mean "all". */
  projectStudioCasesListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCasesListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.kind ?? ''),
      String(payload.status ?? ''),
      String(payload.priority ?? ''),
      String(payload.tagId ?? payload.tag_id ?? ''),
      String(payload.origin ?? ''),
      String(payload.search ?? ''),
      Number(payload.offset ?? 0),
      Number(payload.limit ?? 50),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CaseGetRequest). payload: { projectId, caseId, includeVersions? }. */
  projectStudioCaseGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCaseGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.caseId ?? payload.case_id ?? ''),
      !!(payload.includeVersions ?? payload.include_versions ?? false),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CaseSaveRequest). payload: { projectId, caseId?, kind, title, priority, contentJson, tagIds:[], linkedSourceIds:[], attachmentsJson, expectedVersion?, changeNote? } — caseId null = create. */
  projectStudioCaseSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const caseId = payload.caseId ?? payload.case_id;
    const expectedVersion = payload.expectedVersion ?? payload.expected_version;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      case_id: caseId == null || caseId === '' ? null : String(caseId),
      kind: String(payload.kind ?? ''),
      title: String(payload.title ?? ''),
      priority: String(payload.priority ?? ''),
      content_json: String(payload.contentJson ?? payload.content_json ?? '{}'),
      tag_ids: (Array.isArray(payload.tagIds ?? payload.tag_ids) ? (payload.tagIds ?? payload.tag_ids) : []).map(String),
      linked_source_ids: (Array.isArray(payload.linkedSourceIds ?? payload.linked_source_ids)
        ? (payload.linkedSourceIds ?? payload.linked_source_ids)
        : []
      ).map(String),
      attachments_json: String(payload.attachmentsJson ?? payload.attachments_json ?? '[]'),
      expected_version: expectedVersion == null ? null : Number(expectedVersion),
      change_note: String(payload.changeNote ?? payload.change_note ?? ''),
    };
    const body = _wasm.encodeProjectStudioCaseSaveRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CaseStatusSetRequest). payload: { projectId, caseId, status, reason? } — every downgrade requires reason. */
  projectStudioCaseStatusSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCaseStatusSetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.caseId ?? payload.case_id ?? ''),
      String(payload.status ?? ''),
      String(payload.reason ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CasesBulkStatusRequest). payload: { projectId, caseIds:[], status, reason? }. */
  projectStudioCasesBulkStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      case_ids: (Array.isArray(payload.caseIds ?? payload.case_ids) ? (payload.caseIds ?? payload.case_ids) : []).map(String),
      status: String(payload.status ?? ''),
      reason: String(payload.reason ?? ''),
    };
    const body = _wasm.encodeProjectStudioCasesBulkStatusRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CaseDuplicateRequest). payload: { projectId, caseId }. */
  projectStudioCaseDuplicateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCaseDuplicateRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.caseId ?? payload.case_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CaseDeleteRequest). payload: { projectId, caseId }. */
  projectStudioCaseDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCaseDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.caseId ?? payload.case_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CaseVersionGetRequest). payload: { projectId, caseId, version }. */
  projectStudioCaseVersionGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCaseVersionGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.caseId ?? payload.case_id ?? ''),
      Number(payload.version ?? 0),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CaseRestoreVersionRequest). payload: { projectId, caseId, version, expectedVersion } — restore = NEW version (append-only history). */
  projectStudioCaseRestoreVersionRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCaseRestoreVersionRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.caseId ?? payload.case_id ?? ''),
      Number(payload.version ?? 0),
      Number(payload.expectedVersion ?? payload.expected_version ?? 0),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CasesImportCsvRequest). payload: { projectId, csvText, dryRun? } — server clamps 2 MiB / 500 rows, dryRun only validates. */
  projectStudioCasesImportCsvRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      csv_text: String(payload.csvText ?? payload.csv_text ?? ''),
      dry_run: !!(payload.dryRun ?? payload.dry_run ?? false),
    };
    const body = _wasm.encodeProjectStudioCasesImportCsvRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(AttachmentGetRequest). payload: { projectId, sha256, maxBytes? } — response.bytes arrives as Uint8Array. */
  projectStudioAttachmentGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioAttachmentGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.sha256 ?? ''),
      Number(payload.maxBytes ?? payload.max_bytes ?? 0),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SuitesListRequest). payload: { projectId }. */
  projectStudioSuitesListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSuitesListRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SuiteGetRequest). payload: { projectId, suiteId }. */
  projectStudioSuiteGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSuiteGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.suiteId ?? payload.suite_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SuiteSaveRequest). payload: { projectId, suiteId?, name, description?, caseIds:[] } — suiteId null = create, case order = positions. */
  projectStudioSuiteSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const suiteId = payload.suiteId ?? payload.suite_id;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      suite_id: suiteId == null || suiteId === '' ? null : String(suiteId),
      name: String(payload.name ?? ''),
      description: String(payload.description ?? ''),
      case_ids: (Array.isArray(payload.caseIds ?? payload.case_ids) ? (payload.caseIds ?? payload.case_ids) : []).map(String),
    };
    const body = _wasm.encodeProjectStudioSuiteSaveRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SuiteDeleteRequest). payload: { projectId, suiteId }. */
  projectStudioSuiteDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSuiteDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.suiteId ?? payload.suite_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunsListRequest). payload: { projectId, status?, runType?, offset?, limit? }. */
  projectStudioRunsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunsListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.status ?? ''),
      String(payload.runType ?? payload.run_type ?? ''),
      Number(payload.offset ?? 0),
      Number(payload.limit ?? 50),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunCreateRequest). payload: { projectId, name, suiteId? XOR caseIds? XOR fromFailedRunId?, envNote?, assignmentMode, singleAssignee?, assignments:[{caseId,userId}] }. */
  projectStudioRunCreateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const assignments = (Array.isArray(payload.assignments) ? payload.assignments : []).map((a) => ({
      case_id: String(a.caseId ?? a.case_id ?? ''),
      user_id: String(a.userId ?? a.user_id ?? ''),
    }));
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      name: String(payload.name ?? ''),
      suite_id: String(payload.suiteId ?? payload.suite_id ?? ''),
      case_ids: (Array.isArray(payload.caseIds ?? payload.case_ids) ? (payload.caseIds ?? payload.case_ids) : []).map(String),
      from_failed_run_id: String(payload.fromFailedRunId ?? payload.from_failed_run_id ?? ''),
      env_note: String(payload.envNote ?? payload.env_note ?? ''),
      assignment_mode: String(payload.assignmentMode ?? payload.assignment_mode ?? ''),
      single_assignee: String(payload.singleAssignee ?? payload.single_assignee ?? ''),
      assignments,
    };
    const body = _wasm.encodeProjectStudioRunCreateRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunGetRequest). payload: { projectId, runId }. */
  projectStudioRunGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.runId ?? payload.run_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunCloseRequest). payload: { projectId, runId, cancelled? }. */
  projectStudioRunCloseRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunCloseRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.runId ?? payload.run_id ?? ''),
      !!payload.cancelled,
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunDeleteRequest). payload: { projectId, runId }. */
  projectStudioRunDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.runId ?? payload.run_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunItemClaimRequest). payload: { projectId, runId, itemId? } — itemId omitted = claim nearest pool item (atomic server-side). */
  projectStudioRunItemClaimRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const itemId = payload.itemId ?? payload.item_id;
    const body = _wasm.encodeProjectStudioRunItemClaimRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.runId ?? payload.run_id ?? ''),
      itemId == null || itemId === '' ? undefined : String(itemId),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunItemReleaseRequest). payload: { projectId, itemId }. */
  projectStudioRunItemReleaseRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunItemReleaseRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.itemId ?? payload.item_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunItemGetRequest). payload: { projectId, itemId }. */
  projectStudioRunItemGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunItemGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.itemId ?? payload.item_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunStepSetRequest). payload: { projectId, itemId, stepIndex, status, note?, attachmentsJson? }. */
  projectStudioRunStepSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      item_id: String(payload.itemId ?? payload.item_id ?? ''),
      step_index: Number(payload.stepIndex ?? payload.step_index ?? 0),
      status: String(payload.status ?? ''),
      note: String(payload.note ?? ''),
      attachments_json: String(payload.attachmentsJson ?? payload.attachments_json ?? ''),
    };
    const body = _wasm.encodeProjectStudioRunStepSetRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunItemFinishRequest). payload: { projectId, itemId, status?, resultNote?, testerConfig?, durationSecs, attachmentsJson? } — empty status = server derives the verdict. */
  projectStudioRunItemFinishRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      item_id: String(payload.itemId ?? payload.item_id ?? ''),
      status: String(payload.status ?? ''),
      result_note: String(payload.resultNote ?? payload.result_note ?? ''),
      tester_config: String(payload.testerConfig ?? payload.tester_config ?? ''),
      duration_secs: Number(payload.durationSecs ?? payload.duration_secs ?? 0),
      attachments_json: String(payload.attachmentsJson ?? payload.attachments_json ?? ''),
    };
    const body = _wasm.encodeProjectStudioRunItemFinishRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MyTestWorkRequest) — cross-project, no payload. */
  projectStudioMyTestWorkRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioMyTestWorkRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TasksListRequest). payload: { projectId, taskType?, status?, assignedTo?, search?, offset?, limit? }. */
  projectStudioTasksListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioTasksListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.taskType ?? payload.task_type ?? ''),
      String(payload.status ?? ''),
      String(payload.assignedTo ?? payload.assigned_to ?? ''),
      String(payload.search ?? ''),
      Number(payload.offset ?? 0),
      Number(payload.limit ?? 50),
      payload.severity == null || payload.severity === '' ? undefined : String(payload.severity),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TaskGetRequest). payload: { projectId, taskId }. */
  projectStudioTaskGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioTaskGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.taskId ?? payload.task_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TaskSaveRequest). payload: { projectId, taskId?, taskType, title, descriptionMd?, severity?, priority, status, assignedTo?, dueDate?, linksJson?, attachmentsJson? } — taskId null = create, defects require severity. */
  projectStudioTaskSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const taskId = payload.taskId ?? payload.task_id;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      task_id: taskId == null || taskId === '' ? null : String(taskId),
      task_type: String(payload.taskType ?? payload.task_type ?? ''),
      title: String(payload.title ?? ''),
      description_md: String(payload.descriptionMd ?? payload.description_md ?? ''),
      severity: String(payload.severity ?? ''),
      priority: String(payload.priority ?? ''),
      status: String(payload.status ?? ''),
      assigned_to: String(payload.assignedTo ?? payload.assigned_to ?? ''),
      due_date: String(payload.dueDate ?? payload.due_date ?? ''),
      links_json: String(payload.linksJson ?? payload.links_json ?? ''),
      attachments_json: String(payload.attachmentsJson ?? payload.attachments_json ?? ''),
    };
    const body = _wasm.encodeProjectStudioTaskSaveRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TaskDeleteRequest). payload: { projectId, taskId }. */
  projectStudioTaskDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioTaskDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.taskId ?? payload.task_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TaskCommentAddRequest). payload: { projectId, taskId, bodyMd }. */
  projectStudioTaskCommentAddRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioTaskCommentAddRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.taskId ?? payload.task_id ?? ''),
      String(payload.bodyMd ?? payload.body_md ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TaskCommentEditRequest). payload: { projectId, commentId, bodyMd }. */
  projectStudioTaskCommentEditRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioTaskCommentEditRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.commentId ?? payload.comment_id ?? ''),
      String(payload.bodyMd ?? payload.body_md ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TaskCommentDeleteRequest). payload: { projectId, commentId }. */
  projectStudioTaskCommentDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioTaskCommentDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.commentId ?? payload.comment_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(GenerationStartRequest). payload: { projectId, kind, sourceIds:[], requestedCount?, instructions?, agentId? } — agentId null = project 'generator_manual' binding. */
  projectStudioGenerationStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const agentId = payload.agentId ?? payload.agent_id;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      kind: String(payload.kind ?? ''),
      source_ids: (Array.isArray(payload.sourceIds ?? payload.source_ids) ? (payload.sourceIds ?? payload.source_ids) : []).map(String),
      requested_count: Number(payload.requestedCount ?? payload.requested_count ?? 0),
      instructions: String(payload.instructions ?? ''),
      agent_id: agentId == null || agentId === '' ? null : String(agentId),
    };
    const body = _wasm.encodeProjectStudioGenerationStartRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(GenerationsListRequest). payload: { projectId }. */
  projectStudioGenerationsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioGenerationsListRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(GenerationGetRequest). payload: { projectId, genId } — polling is the progress source of truth. */
  projectStudioGenerationGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioGenerationGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.genId ?? payload.gen_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(GenerationCancelRequest). payload: { projectId, genId }. */
  projectStudioGenerationCancelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioGenerationCancelRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.genId ?? payload.gen_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(GenerationReviewRequest). payload: { projectId, genId, acceptCaseIds:[], rejectCaseIds:[] }. */
  projectStudioGenerationReviewRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      gen_id: String(payload.genId ?? payload.gen_id ?? ''),
      accept_case_ids: (Array.isArray(payload.acceptCaseIds ?? payload.accept_case_ids)
        ? (payload.acceptCaseIds ?? payload.accept_case_ids)
        : []
      ).map(String),
      reject_case_ids: (Array.isArray(payload.rejectCaseIds ?? payload.reject_case_ids)
        ? (payload.rejectCaseIds ?? payload.reject_case_ids)
        : []
      ).map(String),
    };
    const body = _wasm.encodeProjectStudioGenerationReviewRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(GenerationDeleteRequest). payload: { projectId, genId }. */
  projectStudioGenerationDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioGenerationDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.genId ?? payload.gen_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(NotificationsListRequest). payload: { onlyUnread?, beforeId?, limit? } — caller-scoped, no projectId. */
  projectStudioNotificationsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const beforeId = payload.beforeId ?? payload.before_id;
    const body = _wasm.encodeProjectStudioNotificationsListRequest(
      !!(payload.onlyUnread ?? payload.only_unread ?? false),
      beforeId == null || beforeId === '' ? undefined : String(beforeId),
      Number(payload.limit ?? 50),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(NotificationsMarkReadRequest). payload: { notificationIds:[] } — empty = mark ALL as read. */
  projectStudioNotificationsMarkReadRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      notification_ids: (Array.isArray(payload.notificationIds ?? payload.notification_ids)
        ? (payload.notificationIds ?? payload.notification_ids)
        : []
      ).map(String),
    };
    const body = _wasm.encodeProjectStudioNotificationsMarkReadRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ReportQueryRequest). payload: { projectId, report, fromDate?, toDate?, suiteId?, runIds?:[] } — rows_json schema is per report; 'perf_compare' takes exactly two runIds. */
  projectStudioReportQueryRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const runIds = payload.runIds ?? payload.run_ids;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      report: String(payload.report ?? ''),
      from_date: String(payload.fromDate ?? payload.from_date ?? ''),
      to_date: String(payload.toDate ?? payload.to_date ?? ''),
      suite_id: String(payload.suiteId ?? payload.suite_id ?? ''),
      run_ids: (Array.isArray(runIds) ? runIds : []).map(String),
    };
    const body = _wasm.encodeProjectStudioReportQueryRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  // --- Project Studio F3: test environments, build profiles, runners,
  // automated runs, try-run, git/zip/api_spec sources, artifacts, code assist.
  // Multi-field mutating requests (EnvironmentSave, BuildProfileSave,
  // RunStartAuto, TryRunStart) serialize the whole snake_case field set as ONE
  // JSON string parsed by serde in WASM; simple requests keep explicit
  // parameters like the F1/F2 sections above. ---

  /** MessageBody::ProjectStudioBody(EnvironmentsListRequest). payload: { projectId }. */
  projectStudioEnvironmentsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioEnvironmentsListRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(EnvironmentSaveRequest). payload: { projectId, environmentId?, name, envType, baseUrl, authType, secret?, extraHeadersJson?, hostAllowlist:[], justification? } — environmentId null = create; secret null/undefined = keep stored, '' = clear, value = replace. */
  projectStudioEnvironmentSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const environmentId = payload.environmentId ?? payload.environment_id;
    const secret = payload.secret;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      environment_id: environmentId == null || environmentId === '' ? null : String(environmentId),
      name: String(payload.name ?? ''),
      env_type: String(payload.envType ?? payload.env_type ?? ''),
      base_url: String(payload.baseUrl ?? payload.base_url ?? ''),
      auth_type: String(payload.authType ?? payload.auth_type ?? ''),
      secret: secret == null ? null : String(secret),
      extra_headers_json: String(payload.extraHeadersJson ?? payload.extra_headers_json ?? ''),
      host_allowlist: (Array.isArray(payload.hostAllowlist ?? payload.host_allowlist)
        ? (payload.hostAllowlist ?? payload.host_allowlist)
        : []
      ).map(String),
      justification: String(payload.justification ?? ''),
    };
    const body = _wasm.encodeProjectStudioEnvironmentSaveRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(EnvironmentDeleteRequest). payload: { projectId, environmentId }. */
  projectStudioEnvironmentDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioEnvironmentDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.environmentId ?? payload.environment_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(EnvApprovalsListRequest) — admin only, cross-project pending approvals; no payload. */
  projectStudioEnvApprovalsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioEnvApprovalsListRequest();
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(EnvApprovalDecideRequest). payload: { projectId, environmentId, approve, reason? } — rejection requires non-empty reason. */
  projectStudioEnvApprovalDecideRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioEnvApprovalDecideRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.environmentId ?? payload.environment_id ?? ''),
      !!(payload.approve ?? false),
      String(payload.reason ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(BuildProfileGetRequest). payload: { projectId, sourceId }. */
  projectStudioBuildProfileGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioBuildProfileGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.sourceId ?? payload.source_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(BuildProfileSaveRequest). payload: { projectId, sourceId, toolchain, baseImage?, installCmd?, testCmd, workdir? } — upserts the single profile of a source. */
  projectStudioBuildProfileSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      source_id: String(payload.sourceId ?? payload.source_id ?? ''),
      toolchain: String(payload.toolchain ?? ''),
      base_image: String(payload.baseImage ?? payload.base_image ?? ''),
      install_cmd: String(payload.installCmd ?? payload.install_cmd ?? ''),
      test_cmd: String(payload.testCmd ?? payload.test_cmd ?? ''),
      workdir: String(payload.workdir ?? ''),
    };
    const body = _wasm.encodeProjectStudioBuildProfileSaveRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunnersListRequest). payload: { projectId } — test-runner discovery. */
  projectStudioRunnersListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunnersListRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunStartAutoRequest). payload: { projectId, name, suiteId? XOR caseIds? XOR fromRunId?, environmentId, runnerServiceId?, perfProfileJson? } — environment must be approved; empty runnerServiceId lets the server match by toolchain. */
  projectStudioRunStartAutoRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      name: String(payload.name ?? ''),
      suite_id: String(payload.suiteId ?? payload.suite_id ?? ''),
      case_ids: (Array.isArray(payload.caseIds ?? payload.case_ids) ? (payload.caseIds ?? payload.case_ids) : []).map(String),
      from_run_id: String(payload.fromRunId ?? payload.from_run_id ?? ''),
      environment_id: String(payload.environmentId ?? payload.environment_id ?? ''),
      runner_service_id: String(payload.runnerServiceId ?? payload.runner_service_id ?? ''),
      perf_profile_json: String(payload.perfProfileJson ?? payload.perf_profile_json ?? ''),
    };
    const body = _wasm.encodeProjectStudioRunStartAutoRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunAutoGetRequest). payload: { projectId, runId } — polling snapshot, source of truth for automated-run progress. */
  projectStudioRunAutoGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunAutoGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.runId ?? payload.run_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunAutoCancelRequest). payload: { projectId, runId }. */
  projectStudioRunAutoCancelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunAutoCancelRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.runId ?? payload.run_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TryRunStartRequest) — STREAM-INITIATING (no plain response): chunks = TryRunStreamChunk, end = TryRunStreamEnd. payload: { projectId, tryId (client-minted), caseId, environmentId, contentJsonOverride?, language?, perfProfileJson? } — '' override = run the saved case content. */
  projectStudioTryRunStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      try_id: String(payload.tryId ?? payload.try_id ?? ''),
      case_id: String(payload.caseId ?? payload.case_id ?? ''),
      environment_id: String(payload.environmentId ?? payload.environment_id ?? ''),
      content_json_override: String(payload.contentJsonOverride ?? payload.content_json_override ?? ''),
      language: String(payload.language ?? ''),
      perf_profile_json: String(payload.perfProfileJson ?? payload.perf_profile_json ?? ''),
    };
    const body = _wasm.encodeProjectStudioTryRunStartRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TryRunCancelRequest). payload: { projectId, tryId } — addresses the ephemeral execution by the client-minted tryId. */
  projectStudioTryRunCancelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioTryRunCancelRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.tryId ?? payload.try_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceRefreshRequest). payload: { projectId, sourceId } — git sources only: fetch + delta re-index. */
  projectStudioSourceRefreshRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSourceRefreshRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.sourceId ?? payload.source_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ApiSpecEndpointsRequest). payload: { projectId, sourceId } — parsed endpoint list of an api_spec source. */
  projectStudioApiSpecEndpointsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioApiSpecEndpointsRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.sourceId ?? payload.source_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(SourceSecretSetRequest). payload: { projectId, sourceId, token? } — token null/'' = clear the stored git token (input-only, reads never return it). */
  projectStudioSourceSecretSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const token = payload.token;
    const body = _wasm.encodeProjectStudioSourceSecretSetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.sourceId ?? payload.source_id ?? ''),
      token == null || token === '' ? undefined : String(token),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunArtifactGetRequest). payload: { projectId, artifactId, maxBytes? } — server clamps to 32 MiB, response.bytes arrives as Uint8Array. */
  projectStudioRunArtifactGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunArtifactGetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.artifactId ?? payload.artifact_id ?? ''),
      Number(payload.maxBytes ?? payload.max_bytes ?? 0),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(RunAutoStreamRequest). payload: { projectId, runId } — subscribe to the live view of an automated run (chunks = RunAutoStreamChunk, end = RunAutoStreamEnd). */
  projectStudioRunAutoStreamRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioRunAutoStreamRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.runId ?? payload.run_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(CodeAssistRequest) — STREAM-INITIATING: tokens via CodeAssistStreamChunk, final proposal in CodeAssistStreamEnd. payload: { projectId, caseId, kind, selection?, instruction, fullContent } — '' selection = whole script. */
  projectStudioCodeAssistRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioCodeAssistRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.caseId ?? payload.case_id ?? ''),
      String(payload.kind ?? ''),
      String(payload.selection ?? ''),
      String(payload.instruction ?? ''),
      String(payload.fullContent ?? payload.full_content ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  // --- Project Studio F4: run schedules, ML Studio links, kanban status,
  // project export/import. Multi-field mutating requests (ScheduleSave,
  // MlProjectCreateFromProject, MlLinkAttach, MlLinkUpdate, ProjectExportStart,
  // ProjectImportApply) serialize the whole snake_case field set as ONE JSON
  // string parsed by serde in WASM; simple requests keep explicit parameters
  // like the F1/F2/F3 sections above. ---

  /** MessageBody::ProjectStudioBody(SchedulesListRequest). payload: { projectId } — response also carries the node's serverTimezone. */
  projectStudioSchedulesListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioSchedulesListRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ScheduleSaveRequest). payload: { projectId, scheduleId?, name, runType, suiteId?, caseIds?:[], environmentId?, runnerServiceId?, perfProfileJson?, assignmentMode?, assignees?:[], scheduleKind, scheduleExpr, timezone?, enabled } — scheduleId null = create; this is the COMPLETE definition, every omitted field is a real clear (use projectStudioScheduleSetEnabledRequest for the row toggle). */
  projectStudioScheduleSaveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const scheduleId = payload.scheduleId ?? payload.schedule_id;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      schedule_id: scheduleId == null || scheduleId === '' ? null : String(scheduleId),
      name: String(payload.name ?? ''),
      run_type: String(payload.runType ?? payload.run_type ?? ''),
      suite_id: String(payload.suiteId ?? payload.suite_id ?? ''),
      case_ids: (Array.isArray(payload.caseIds ?? payload.case_ids) ? (payload.caseIds ?? payload.case_ids) : []).map(String),
      environment_id: String(payload.environmentId ?? payload.environment_id ?? ''),
      runner_service_id: String(payload.runnerServiceId ?? payload.runner_service_id ?? ''),
      perf_profile_json: String(payload.perfProfileJson ?? payload.perf_profile_json ?? ''),
      assignment_mode: String(payload.assignmentMode ?? payload.assignment_mode ?? ''),
      assignees: (Array.isArray(payload.assignees) ? payload.assignees : []).map(String),
      schedule_kind: String(payload.scheduleKind ?? payload.schedule_kind ?? ''),
      schedule_expr: String(payload.scheduleExpr ?? payload.schedule_expr ?? ''),
      timezone: String(payload.timezone ?? ''),
      enabled: !!(payload.enabled ?? false),
    };
    const body = _wasm.encodeProjectStudioScheduleSaveRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ScheduleDeleteRequest). payload: { projectId, scheduleId }. */
  projectStudioScheduleDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioScheduleDeleteRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.scheduleId ?? payload.schedule_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ScheduleSetEnabledRequest). payload: { projectId, scheduleId, enabled } — row toggle; re-enabling recomputes nextRunAt and clears the breaker. */
  projectStudioScheduleSetEnabledRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioScheduleSetEnabledRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.scheduleId ?? payload.schedule_id ?? ''),
      !!(payload.enabled ?? false),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ScheduleRunNowRequest). payload: { projectId, scheduleId } — fires through the same gate chain as the loop, never moves nextRunAt. */
  projectStudioScheduleRunNowRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioScheduleRunNowRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.scheduleId ?? payload.schedule_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ScheduleRunsListRequest). payload: { projectId, scheduleId, limit? } — trigger history of one schedule. */
  projectStudioScheduleRunsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioScheduleRunsListRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.scheduleId ?? payload.schedule_id ?? ''),
      Number(payload.limit ?? 0),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MlLinksListRequest). payload: { projectId } — response carries canManage for the create/attach/detach actions. */
  projectStudioMlLinksListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioMlLinksListRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MlProjectCreateFromProjectRequest). payload: { projectId, mlName, projectType, roleMap:[{projectRole, mlRole}], syncPermissions, label? }. */
  projectStudioMlProjectCreateFromProjectRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const roleMap = payload.roleMap ?? payload.role_map;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      ml_name: String(payload.mlName ?? payload.ml_name ?? ''),
      project_type: String(payload.projectType ?? payload.project_type ?? ''),
      role_map: (Array.isArray(roleMap) ? roleMap : []).map((r) => ({
        project_role: String(r.projectRole ?? r.project_role ?? ''),
        ml_role: String(r.mlRole ?? r.ml_role ?? ''),
      })),
      sync_permissions: !!(payload.syncPermissions ?? payload.sync_permissions ?? false),
      label: String(payload.label ?? ''),
    };
    const body = _wasm.encodeProjectStudioMlProjectCreateFromProjectRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MlProjectCandidatesRequest). payload: { projectId } — ML projects the caller OWNS and that are not linked yet. */
  projectStudioMlProjectCandidatesRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioMlProjectCandidatesRequest(String(payload.projectId ?? payload.project_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MlLinkAttachRequest). payload: { projectId, mlProjectId, label?, syncPermissions, roleMap?:[{projectRole, mlRole}] }. */
  projectStudioMlLinkAttachRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const roleMap = payload.roleMap ?? payload.role_map;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      ml_project_id: String(payload.mlProjectId ?? payload.ml_project_id ?? ''),
      label: String(payload.label ?? ''),
      sync_permissions: !!(payload.syncPermissions ?? payload.sync_permissions ?? false),
      role_map: (Array.isArray(roleMap) ? roleMap : []).map((r) => ({
        project_role: String(r.projectRole ?? r.project_role ?? ''),
        ml_role: String(r.mlRole ?? r.ml_role ?? ''),
      })),
    };
    const body = _wasm.encodeProjectStudioMlLinkAttachRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MlLinkUpdateRequest). payload: { projectId, linkId, label?, syncPermissions, roleMap?:[{projectRole, mlRole}] }. */
  projectStudioMlLinkUpdateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const roleMap = payload.roleMap ?? payload.role_map;
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      link_id: String(payload.linkId ?? payload.link_id ?? ''),
      label: String(payload.label ?? ''),
      sync_permissions: !!(payload.syncPermissions ?? payload.sync_permissions ?? false),
      role_map: (Array.isArray(roleMap) ? roleMap : []).map((r) => ({
        project_role: String(r.projectRole ?? r.project_role ?? ''),
        ml_role: String(r.mlRole ?? r.ml_role ?? ''),
      })),
    };
    const body = _wasm.encodeProjectStudioMlLinkUpdateRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MlLinkDetachRequest). payload: { projectId, linkId, revokeMembers } — revokeMembers also drops the ML memberships this link granted; the ML project is never deleted. */
  projectStudioMlLinkDetachRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioMlLinkDetachRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.linkId ?? payload.link_id ?? ''),
      !!(payload.revokeMembers ?? payload.revoke_members ?? false),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(MlLinkSyncNowRequest). payload: { projectId, linkId }. */
  projectStudioMlLinkSyncNowRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioMlLinkSyncNowRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.linkId ?? payload.link_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(TaskStatusSetRequest). payload: { projectId, taskId, status } — status-only kanban move; TaskSave would clear descriptionMd and attachments. */
  projectStudioTaskStatusSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioTaskStatusSetRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.taskId ?? payload.task_id ?? ''),
      String(payload.status ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectExportStartRequest). payload: { projectId, includeRuns, includeVectors, includeUserNames } — includeUserNames copies display names into the archive (personal data, audited). */
  projectStudioProjectExportStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      project_id: String(payload.projectId ?? payload.project_id ?? ''),
      include_runs: !!(payload.includeRuns ?? payload.include_runs ?? false),
      include_vectors: !!(payload.includeVectors ?? payload.include_vectors ?? false),
      include_user_names: !!(payload.includeUserNames ?? payload.include_user_names ?? false),
    };
    const body = _wasm.encodeProjectStudioProjectExportStartRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectExportStatusRequest). payload: { projectId, jobId } — polling is the source of truth; the finished archive is downloaded over the returned signedUrl, not the binary protocol. */
  projectStudioProjectExportStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioProjectExportStatusRequest(
      String(payload.projectId ?? payload.project_id ?? ''),
      String(payload.jobId ?? payload.job_id ?? ''),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectImportUploadChunkRequest). payload: { uploadId (client-minted), filename, seq, totalChunks, bytes:Uint8Array } — no projectId: the project does not exist yet. */
  projectStudioProjectImportUploadChunkRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const raw = payload.bytes;
    const bytes = raw instanceof Uint8Array ? raw : new Uint8Array(raw ?? []);
    const body = _wasm.encodeProjectStudioProjectImportUploadChunkRequest(
      String(payload.uploadId ?? payload.upload_id ?? ''),
      String(payload.filename ?? ''),
      Number(payload.seq ?? 0),
      Number(payload.totalChunks ?? payload.total_chunks ?? 0),
      bytes,
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectImportPreviewRequest). payload: { uploadId } — reads ONLY the archive manifest, nothing is unpacked before confirmation. */
  projectStudioProjectImportPreviewRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioProjectImportPreviewRequest(String(payload.uploadId ?? payload.upload_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectImportApplyRequest). payload: { uploadId, nameOverride?, importVectors, importRuns }. */
  projectStudioProjectImportApplyRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      upload_id: String(payload.uploadId ?? payload.upload_id ?? ''),
      name_override: String(payload.nameOverride ?? payload.name_override ?? ''),
      import_vectors: !!(payload.importVectors ?? payload.import_vectors ?? false),
      import_runs: !!(payload.importRuns ?? payload.import_runs ?? false),
    };
    const body = _wasm.encodeProjectStudioProjectImportApplyRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ProjectImportStatusRequest). payload: { jobId } — addressed by jobId alone: the project row exists only once the import succeeds. */
  projectStudioProjectImportStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioProjectImportStatusRequest(String(payload.jobId ?? payload.job_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::ProjectStudioBody(ArchiveStreamRequest) — STREAM-INITIATING (no plain response): chunks = ArchiveStreamChunk, end = ArchiveStreamEnd. payload: { jobId } — live progress of an export or import job, job owner only. */
  projectStudioArchiveStreamRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeProjectStudioArchiveStreamRequest(String(payload.jobId ?? payload.job_id ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  // ---------------------------------------------------------------------------
  // CodeStudioBody — "Code Studio" module: the workspace registry and sessions,
  // then everything a session needs: filesystem, git broker, patch review,
  // timeline, operation journal, approvals and grants, runs, exec and terminal,
  // workspace settings and the semantic index.
  //
  // The registry and session-lifecycle requests below take positional arguments,
  // mirroring their wasm_bindgen signatures. Everything from FileTreeRequest on
  // crosses to WASM as ONE snake_case JSON string parsed by serde into the enum
  // variant (same policy as the Project Studio F2 requests), so a field appended
  // to the protocol needs no new argument on this side. Paths are always
  // relative to the session worktree — the wire has no host paths.
  // ---------------------------------------------------------------------------

  /** MessageBody::CodeStudioBody(WorkspacesListRequest). payload: { includeArchived } — answers with workspaces + the caller's create grant + the node picker. */
  codeStudioWorkspacesListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioWorkspacesListRequest(
      !!(payload.includeArchived ?? payload.include_archived ?? false),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /**
   * MessageBody::CodeStudioBody(WorkspaceCreateRequest). payload: { name, nodeId,
   * execMode, containerImage?, repoKind, repoUrl?, repoAuthKind?, secretMaterial?,
   * sshHostFingerprint?, defaultBranch?, autonomyCeiling, egressPolicy, indexEnabled,
   * members:[{userId,role}] }. `secretMaterial` travels ONCE and never comes back;
   * the answer only carries the id and the `provisioning` status.
   */
  codeStudioWorkspaceCreateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const members = Array.isArray(payload.members) ? payload.members : [];
    const membersJson = JSON.stringify(members.map((m) => ({
      user_id: csText(m.userId ?? m.user_id),
      role: csText(m.role, 'viewer'),
    })));
    const body = _wasm.encodeCodeStudioWorkspaceCreateRequest(
      csText(payload.name),
      csText(payload.nodeId ?? payload.node_id),
      csText(payload.execMode ?? payload.exec_mode, 'trusted_native'),
      csOptText(payload.containerImage ?? payload.container_image),
      csText(payload.repoKind ?? payload.repo_kind, 'empty'),
      csOptText(payload.repoUrl ?? payload.repo_url),
      csOptText(payload.repoAuthKind ?? payload.repo_auth_kind),
      csOptText(payload.secretMaterial ?? payload.secret_material),
      csOptText(payload.sshHostFingerprint ?? payload.ssh_host_fingerprint),
      csOptText(payload.defaultBranch ?? payload.default_branch),
      csText(payload.autonomyCeiling ?? payload.autonomy_ceiling, 'normal'),
      csText(payload.egressPolicy ?? payload.egress_policy, 'org_approved'),
      !!(payload.indexEnabled ?? payload.index_enabled ?? false),
      membersJson,
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceGetRequest). payload: { workspaceId } — detail + members + provisioning steps. */
  codeStudioWorkspaceGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioWorkspaceGetRequest(csText(payload.workspaceId ?? payload.workspace_id));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceRetryRequest). payload: { workspaceId } — resumes provisioning; `done` steps are skipped. */
  codeStudioWorkspaceRetryRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioWorkspaceRetryRequest(csText(payload.workspaceId ?? payload.workspace_id));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceArchiveRequest). payload: { workspaceId, archived }. */
  codeStudioWorkspaceArchiveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioWorkspaceArchiveRequest(
      csText(payload.workspaceId ?? payload.workspace_id),
      !!(payload.archived ?? false),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceMemberSetRequest). payload: { workspaceId, userId, role } — adds or re-roles a member. */
  codeStudioWorkspaceMemberSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioWorkspaceMemberSetRequest(
      csText(payload.workspaceId ?? payload.workspace_id),
      csText(payload.userId ?? payload.user_id),
      csText(payload.role, 'viewer'),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceMemberRemoveRequest). payload: { workspaceId, userId }. */
  codeStudioWorkspaceMemberRemoveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioWorkspaceMemberRemoveRequest(
      csText(payload.workspaceId ?? payload.workspace_id),
      csText(payload.userId ?? payload.user_id),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceCreatorGrantSetRequest). payload: { userId, granted } — the per-user right to create workspaces. */
  codeStudioWorkspaceCreatorGrantSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioWorkspaceCreatorGrantSetRequest(
      csText(payload.userId ?? payload.user_id),
      !!(payload.granted ?? false),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionsListRequest). payload: { workspaceId } — the CALLER's sessions; the server filters by user with no admin bypass. */
  codeStudioSessionsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioSessionsListRequest(csText(payload.workspaceId ?? payload.workspace_id));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionOpenRequest). payload: { workspaceId, title, autonomyMode } — the branch is derived server-side, never sent from the UI. */
  codeStudioSessionOpenRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioSessionOpenRequest(
      csText(payload.workspaceId ?? payload.workspace_id),
      csText(payload.title),
      csText(payload.autonomyMode ?? payload.autonomy_mode, 'normal'),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionCloseRequest). payload: { workspaceId, sessionId } — drops the worktree, keeps the branch. */
  codeStudioSessionCloseRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioSessionCloseRequest(
      csText(payload.workspaceId ?? payload.workspace_id),
      csText(payload.sessionId ?? payload.session_id),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(FileTreeRequest). payload: { workspaceId, sessionId, path, depth }. */
  codeStudioFileTreeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      path: csText(payload.path),
      depth: Number(payload.depth ?? 1),
    };
    const body = _wasm.encodeCodeStudioFileTreeRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(FileReadRequest). payload: { workspaceId, sessionId, path, startLine?, endLine? } — 1-based inclusive range, both omitted = whole file. */
  codeStudioFileReadRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const startLine = payload.startLine ?? payload.start_line;
    const endLine = payload.endLine ?? payload.end_line;
    const request = {
      ...csScope(payload),
      path: csText(payload.path),
      start_line: startLine == null || startLine === '' ? null : Number(startLine),
      end_line: endLine == null || endLine === '' ? null : Number(endLine),
    };
    const body = _wasm.encodeCodeStudioFileReadRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(FileWriteRequest). payload: { workspaceId, sessionId, path, content, expectedBlobSha? } — omitted sha means the file must not exist yet. */
  codeStudioFileWriteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      path: csText(payload.path),
      content: csText(payload.content),
      expected_blob_sha: csOptText(payload.expectedBlobSha ?? payload.expected_blob_sha),
    };
    const body = _wasm.encodeCodeStudioFileWriteRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(FileCreateRequest). payload: { workspaceId, sessionId, path, content }. */
  codeStudioFileCreateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      path: csText(payload.path),
      content: csText(payload.content),
    };
    const body = _wasm.encodeCodeStudioFileCreateRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(FileDeleteRequest). payload: { workspaceId, sessionId, path, recursive, expectedBlobSha? }. */
  codeStudioFileDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      path: csText(payload.path),
      recursive: !!(payload.recursive ?? false),
      expected_blob_sha: csOptText(payload.expectedBlobSha ?? payload.expected_blob_sha),
    };
    const body = _wasm.encodeCodeStudioFileDeleteRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(FileRenameRequest). payload: { workspaceId, sessionId, fromPath, toPath, expectedBlobSha? }. */
  codeStudioFileRenameRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      from_path: csText(payload.fromPath ?? payload.from_path),
      to_path: csText(payload.toPath ?? payload.to_path),
      expected_blob_sha: csOptText(payload.expectedBlobSha ?? payload.expected_blob_sha),
    };
    const body = _wasm.encodeCodeStudioFileRenameRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(FileMkdirRequest). payload: { workspaceId, sessionId, path }. */
  codeStudioFileMkdirRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { ...csScope(payload), path: csText(payload.path) };
    const body = _wasm.encodeCodeStudioFileMkdirRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(FileGrepRequest). payload: { workspaceId, sessionId, query, glob, regex, maxResults } — regex=false keeps the query literal. */
  codeStudioFileGrepRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      query: csText(payload.query),
      glob: csText(payload.glob),
      regex: !!(payload.regex ?? false),
      max_results: Number(payload.maxResults ?? payload.max_results ?? 200),
    };
    const body = _wasm.encodeCodeStudioFileGrepRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitStatusRequest). payload: { workspaceId, sessionId }. */
  codeStudioGitStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioGitStatusRequest(JSON.stringify(csScope(payload)));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitLogRequest). payload: { workspaceId, sessionId, path, limit } — empty path = whole branch. */
  codeStudioGitLogRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      path: csText(payload.path),
      limit: Number(payload.limit ?? 50),
    };
    const body = _wasm.encodeCodeStudioGitLogRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitBranchesRequest). payload: { workspaceId, sessionId }. */
  codeStudioGitBranchesRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioGitBranchesRequest(JSON.stringify(csScope(payload)));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitDiffRequest). payload: { workspaceId, sessionId, path, staged, base } — empty base = the session base commit. */
  codeStudioGitDiffRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      path: csText(payload.path),
      staged: !!(payload.staged ?? false),
      base: csText(payload.base),
    };
    const body = _wasm.encodeCodeStudioGitDiffRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitCommitRequest). payload: { workspaceId, sessionId, message, patchSetId? } — no patch set opens the review gate instead of committing. */
  codeStudioGitCommitRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      message: csText(payload.message),
      patch_set_id: csOptText(payload.patchSetId ?? payload.patch_set_id),
    };
    const body = _wasm.encodeCodeStudioGitCommitRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitPushRequest). payload: { workspaceId, sessionId, remote, setUpstream } — mandatory-interactive, the answer may be an approval question. */
  codeStudioGitPushRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      remote: csText(payload.remote, 'origin'),
      set_upstream: !!(payload.setUpstream ?? payload.set_upstream ?? false),
    };
    const body = _wasm.encodeCodeStudioGitPushRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitSyncRequest). payload: { workspaceId, sessionId, mode } — 'fetch' | 'pull'. */
  codeStudioGitSyncRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { ...csScope(payload), mode: csText(payload.mode, 'fetch') };
    const body = _wasm.encodeCodeStudioGitSyncRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitMergeRequest). payload: { workspaceId, sessionId, sourceBranch, targetBranch } — a conflict comes back as a result, not an error. */
  codeStudioGitMergeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      source_branch: csText(payload.sourceBranch ?? payload.source_branch),
      target_branch: csText(payload.targetBranch ?? payload.target_branch),
    };
    const body = _wasm.encodeCodeStudioGitMergeRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitMergeFinalizeRequest). payload: { workspaceId, sessionId, opId, patchSetId }. */
  codeStudioGitMergeFinalizeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      op_id: csText(payload.opId ?? payload.op_id),
      patch_set_id: csText(payload.patchSetId ?? payload.patch_set_id),
    };
    const body = _wasm.encodeCodeStudioGitMergeFinalizeRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(GitMergeAbandonRequest). payload: { workspaceId, sessionId, opId } — drops a held integration worktree and its private ref. */
  codeStudioGitMergeAbandonRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { ...csScope(payload), op_id: csText(payload.opId ?? payload.op_id) };
    const body = _wasm.encodeCodeStudioGitMergeAbandonRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorktreesListRequest). payload: { workspaceId, sessionId }. */
  codeStudioWorktreesListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioWorktreesListRequest(JSON.stringify(csScope(payload)));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(PatchSetsListRequest). payload: { workspaceId, sessionId, status } — empty status = every status. */
  codeStudioPatchSetsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { ...csScope(payload), status: csText(payload.status) };
    const body = _wasm.encodeCodeStudioPatchSetsListRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(PatchSetGetRequest). payload: { workspaceId, sessionId, patchSetId } — files with their hunks. */
  codeStudioPatchSetGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      patch_set_id: csText(payload.patchSetId ?? payload.patch_set_id),
    };
    const body = _wasm.encodeCodeStudioPatchSetGetRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(PatchDecideRequest). payload: { workspaceId, sessionId, patchSetId, files:[{ patchFileId, decision, note, hunks:[{ patchHunkId, decision }] }] } — decision: accept|reject|request_revision. */
  codeStudioPatchDecideRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const files = (Array.isArray(payload.files) ? payload.files : []).map((f) => ({
      patch_file_id: csText(f.patchFileId ?? f.patch_file_id),
      decision: csText(f.decision),
      note: csOptText(f.note),
      hunks: (Array.isArray(f.hunks) ? f.hunks : []).map((h) => ({
        patch_hunk_id: csText(h.patchHunkId ?? h.patch_hunk_id),
        decision: csText(h.decision),
      })),
    }));
    const request = {
      ...csScope(payload),
      patch_set_id: csText(payload.patchSetId ?? payload.patch_set_id),
      files,
    };
    const body = _wasm.encodeCodeStudioPatchDecideRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(PatchSetAbandonRequest). payload: { workspaceId, sessionId, patchSetId }. */
  codeStudioPatchSetAbandonRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      patch_set_id: csText(payload.patchSetId ?? payload.patch_set_id),
    };
    const body = _wasm.encodeCodeStudioPatchSetAbandonRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionTimelineRequest). payload: { workspaceId, sessionId, afterSeq, limit } — afterSeq is the resume cursor. */
  codeStudioSessionTimelineRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      after_seq: Number(payload.afterSeq ?? payload.after_seq ?? 0),
      limit: Number(payload.limit ?? 100),
    };
    const body = _wasm.encodeCodeStudioSessionTimelineRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionOperationsRequest). payload: { workspaceId, sessionId, status, limit } — status='unknown' lists what needs a human. */
  codeStudioSessionOperationsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      status: csText(payload.status),
      limit: Number(payload.limit ?? 100),
    };
    const body = _wasm.encodeCodeStudioSessionOperationsRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(OperationResolveRequest). payload: { workspaceId, sessionId, opId, resolution, note } — resolution: completed|failed. */
  codeStudioOperationResolveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      op_id: csText(payload.opId ?? payload.op_id),
      resolution: csText(payload.resolution),
      note: csText(payload.note),
    };
    const body = _wasm.encodeCodeStudioOperationResolveRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(ApprovalsListRequest). payload: { workspaceId, sessionId, status }. */
  codeStudioApprovalsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { ...csScope(payload), status: csText(payload.status) };
    const body = _wasm.encodeCodeStudioApprovalsListRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(ApprovalDecideRequest). payload: { workspaceId, sessionId, approvalId, decision } — allow_once|allow_for_run|allow_for_session|always|deny. */
  codeStudioApprovalDecideRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      approval_id: csText(payload.approvalId ?? payload.approval_id),
      decision: csText(payload.decision),
    };
    const body = _wasm.encodeCodeStudioApprovalDecideRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionGrantsListRequest). payload: { workspaceId, sessionId }. */
  codeStudioSessionGrantsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioSessionGrantsListRequest(JSON.stringify(csScope(payload)));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionGrantRevokeRequest). payload: { workspaceId, sessionId, capability, pattern }. */
  codeStudioSessionGrantRevokeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      capability: csText(payload.capability),
      pattern: csText(payload.pattern),
    };
    const body = _wasm.encodeCodeStudioSessionGrantRevokeRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceAllowlistListRequest). payload: { workspaceId } — the workspace-wide 'always' scope. */
  codeStudioWorkspaceAllowlistListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { workspace_id: csText(payload.workspaceId ?? payload.workspace_id) };
    const body = _wasm.encodeCodeStudioWorkspaceAllowlistListRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceAllowlistSetRequest). payload: { workspaceId, capability, pattern }. */
  codeStudioWorkspaceAllowlistSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      workspace_id: csText(payload.workspaceId ?? payload.workspace_id),
      capability: csText(payload.capability),
      pattern: csText(payload.pattern),
    };
    const body = _wasm.encodeCodeStudioWorkspaceAllowlistSetRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceAllowlistRemoveRequest). payload: { workspaceId, capability, pattern }. */
  codeStudioWorkspaceAllowlistRemoveRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      workspace_id: csText(payload.workspaceId ?? payload.workspace_id),
      capability: csText(payload.capability),
      pattern: csText(payload.pattern),
    };
    const body = _wasm.encodeCodeStudioWorkspaceAllowlistRemoveRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionRunsRequest). payload: { workspaceId, sessionId } — the revision chain with the trigger of every turn. */
  codeStudioSessionRunsRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioSessionRunsRequest(JSON.stringify(csScope(payload)));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionTasksRequest). payload: { workspaceId, sessionId } — the session's plan, the rows the build loop's gate checks. */
  codeStudioSessionTasksRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeCodeStudioSessionTasksRequest(JSON.stringify(csScope(payload)));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionMessageSendRequest). payload: { workspaceId, sessionId, message } — a user turn to the session's root agent. */
  codeStudioSessionMessageSendRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { ...csScope(payload), message: csText(payload.message) };
    const body = _wasm.encodeCodeStudioSessionMessageSendRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionCancelRequest). payload: { workspaceId, sessionId, runId? } — no runId cancels the whole session. */
  codeStudioSessionCancelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { ...csScope(payload), run_id: csOptText(payload.runId ?? payload.run_id) };
    const body = _wasm.encodeCodeStudioSessionCancelRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(SessionAutonomySetRequest). payload: { workspaceId, sessionId, autonomyMode } — raising is clamped to the workspace ceiling server-side. */
  codeStudioSessionAutonomySetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      autonomy_mode: csText(payload.autonomyMode ?? payload.autonomy_mode),
    };
    const body = _wasm.encodeCodeStudioSessionAutonomySetRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(ExecStartRequest). payload: { workspaceId, sessionId, argv:[], cwd, timeoutSecs, mountAccess, networkAccess, ephemeral } — argv is always a vector. */
  codeStudioExecStartRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const argv = (Array.isArray(payload.argv) ? payload.argv : []).map(String);
    const request = {
      ...csScope(payload),
      argv,
      cwd: csText(payload.cwd),
      timeout_secs: Number(payload.timeoutSecs ?? payload.timeout_secs ?? 300),
      mount_access: csText(payload.mountAccess ?? payload.mount_access, 'cow'),
      network_access: csText(payload.networkAccess ?? payload.network_access, 'none'),
      ephemeral: !!(payload.ephemeral ?? false),
    };
    const body = _wasm.encodeCodeStudioExecStartRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(ExecCancelRequest). payload: { workspaceId, sessionId, execId }. */
  codeStudioExecCancelRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { ...csScope(payload), exec_id: csText(payload.execId ?? payload.exec_id) };
    const body = _wasm.encodeCodeStudioExecCancelRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(ExecOutputRequest). payload: { workspaceId, sessionId, execId, afterSeq, limit } — afterSeq is a line cursor. */
  codeStudioExecOutputRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      exec_id: csText(payload.execId ?? payload.exec_id),
      after_seq: Number(payload.afterSeq ?? payload.after_seq ?? 0),
      limit: Number(payload.limit ?? 200),
    };
    const body = _wasm.encodeCodeStudioExecOutputRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(TerminalOpenRequest). payload: { workspaceId, sessionId, rows, cols } — the VT machine runs on the owner node. */
  codeStudioTerminalOpenRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      rows: Number(payload.rows ?? 24),
      cols: Number(payload.cols ?? 80),
    };
    const body = _wasm.encodeCodeStudioTerminalOpenRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(TerminalInputRequest). payload: { workspaceId, sessionId, terminalId, data }. */
  codeStudioTerminalInputRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      terminal_id: csText(payload.terminalId ?? payload.terminal_id),
      data: csText(payload.data),
    };
    const body = _wasm.encodeCodeStudioTerminalInputRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(TerminalResizeRequest). payload: { workspaceId, sessionId, terminalId, rows, cols }. */
  codeStudioTerminalResizeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      terminal_id: csText(payload.terminalId ?? payload.terminal_id),
      rows: Number(payload.rows ?? 24),
      cols: Number(payload.cols ?? 80),
    };
    const body = _wasm.encodeCodeStudioTerminalResizeRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(TerminalCloseRequest). payload: { workspaceId, sessionId, terminalId }. */
  codeStudioTerminalCloseRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      terminal_id: csText(payload.terminalId ?? payload.terminal_id),
    };
    const body = _wasm.encodeCodeStudioTerminalCloseRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(TerminalSnapshotRequest). payload: { workspaceId, sessionId, terminalId } — full grid after a reload; the stream carries only changed rows. */
  codeStudioTerminalSnapshotRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      terminal_id: csText(payload.terminalId ?? payload.terminal_id),
    };
    const body = _wasm.encodeCodeStudioTerminalSnapshotRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceSettingsUpdateRequest). payload: { workspaceId, name, autonomyCeiling, egressPolicy, targetBranch?, indexEnabled, quotaDiskBytes?, quotaSessions? } — execMode is immutable and has no field. */
  codeStudioWorkspaceSettingsUpdateRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const quotaDisk = payload.quotaDiskBytes ?? payload.quota_disk_bytes;
    const quotaSessions = payload.quotaSessions ?? payload.quota_sessions;
    const request = {
      workspace_id: csText(payload.workspaceId ?? payload.workspace_id),
      name: csText(payload.name),
      autonomy_ceiling: csText(payload.autonomyCeiling ?? payload.autonomy_ceiling),
      egress_policy: csText(payload.egressPolicy ?? payload.egress_policy),
      target_branch: csOptText(payload.targetBranch ?? payload.target_branch),
      index_enabled: !!(payload.indexEnabled ?? payload.index_enabled ?? false),
      quota_disk_bytes: quotaDisk == null || quotaDisk === '' ? null : Number(quotaDisk),
      quota_sessions: quotaSessions == null || quotaSessions === '' ? null : Number(quotaSessions),
    };
    const body = _wasm.encodeCodeStudioWorkspaceSettingsUpdateRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceSecretSetRequest). payload: { workspaceId, repoAuthKind, secretMaterial?, sshHostFingerprint? } — material travels once and never comes back. */
  codeStudioWorkspaceSecretSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      workspace_id: csText(payload.workspaceId ?? payload.workspace_id),
      repo_auth_kind: csText(payload.repoAuthKind ?? payload.repo_auth_kind, 'none'),
      secret_material: csOptText(payload.secretMaterial ?? payload.secret_material),
      ssh_host_fingerprint: csOptText(payload.sshHostFingerprint ?? payload.ssh_host_fingerprint),
    };
    const body = _wasm.encodeCodeStudioWorkspaceSecretSetRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceDeleteRequest). payload: { workspaceId }. */
  codeStudioWorkspaceDeleteRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { workspace_id: csText(payload.workspaceId ?? payload.workspace_id) };
    const body = _wasm.encodeCodeStudioWorkspaceDeleteRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(IndexStatusRequest). payload: { workspaceId } — index state per branch. */
  codeStudioIndexStatusRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { workspace_id: csText(payload.workspaceId ?? payload.workspace_id) };
    const body = _wasm.encodeCodeStudioIndexStatusRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(IndexRebuildRequest). payload: { workspaceId, branch }. */
  codeStudioIndexRebuildRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      workspace_id: csText(payload.workspaceId ?? payload.workspace_id),
      branch: csText(payload.branch),
    };
    const body = _wasm.encodeCodeStudioIndexRebuildRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(CodeSearchRequest). payload: { workspaceId, sessionId, query, pathPrefix, limit, mode } — a semantic request may answer with grep and degraded=true. */
  codeStudioCodeSearchRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      query: csText(payload.query),
      path_prefix: csText(payload.pathPrefix ?? payload.path_prefix),
      limit: Number(payload.limit ?? 20),
      mode: csText(payload.mode, 'semantic'),
    };
    const body = _wasm.encodeCodeStudioCodeSearchRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** Subscribe — live session timeline. payload: { workspaceId, sessionId, afterSeq } — chunks = CodeStudioSessionStreamEvent, end = CodeStudioSessionStreamEnd. `afterSeq` is the resume cursor: pass the last `seq` rendered and the stream continues without a gap or a repeat; 0 replays from the first event. */
  codeStudioSessionStreamRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      after_seq: Number(payload.afterSeq ?? payload.after_seq ?? 0),
    };
    const body = _wasm.encodeCodeStudioSessionStreamRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** Subscribe — live terminal grid. payload: { workspaceId, sessionId, terminalId, afterRevision } — chunks = CodeStudioTerminalStreamSnapshot / CodeStudioTerminalStreamDelta, end = CodeStudioTerminalStreamEnd. Pass the revision already rendered to skip the snapshot; anything else earns a full grid first. Cells arrive parsed — the VT machine runs on the owner node. */
  codeStudioTerminalStreamRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      terminal_id: csText(payload.terminalId ?? payload.terminal_id),
      after_revision: Number(payload.afterRevision ?? payload.after_revision ?? 0),
    };
    const body = _wasm.encodeCodeStudioTerminalStreamRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** Subscribe — indexing progress. payload: { workspaceId, afterSeq } — end = CodeStudioIndexStreamEnd. The semantic index is phase 7, so the stream closes with reason 'index_unavailable' rather than reporting progress nothing produces. */
  codeStudioIndexStreamRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      workspace_id: csText(payload.workspaceId ?? payload.workspace_id),
      after_seq: Number(payload.afterSeq ?? payload.after_seq ?? 0),
    };
    const body = _wasm.encodeCodeStudioIndexStreamRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(PatchBlobGetRequest). payload: { workspaceId, sessionId, blobSha } — one CAS blob, so a partially accepted file is rebuilt whole instead of from hunk windows. */
  codeStudioPatchBlobGetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      ...csScope(payload),
      blob_sha: csText(payload.blobSha ?? payload.blob_sha),
    };
    const body = _wasm.encodeCodeStudioPatchBlobGetRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(TerminalsListRequest). payload: { workspaceId, sessionId } — shells already open in the session, so a reload rebuilds the dock instead of opening a second shell. */
  codeStudioTerminalsListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = csScope(payload);
    const body = _wasm.encodeCodeStudioTerminalsListRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(WorkspaceMemberCandidatesRequest). payload: { workspaceId?, query, limit } — omit `workspaceId` in the creation wizard, which has no workspace whose members could be excluded yet; pass it on the member tab and the answer leaves the current members out. */
  codeStudioWorkspaceMemberCandidatesRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      workspace_id: csOptText(payload.workspaceId ?? payload.workspace_id),
      query: csText(payload.query),
      limit: Number(payload.limit ?? 20),
    };
    const body = _wasm.encodeCodeStudioWorkspaceMemberCandidatesRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(ProjectLinkListRequest). payload: { workspaceId } — projects this workspace is linked to. */
  codeStudioProjectLinkListRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = { workspace_id: csText(payload.workspaceId ?? payload.workspace_id) };
    const body = _wasm.encodeCodeStudioProjectLinkListRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(ProjectLinkSetRequest). payload: { workspaceId, projectId, linked } — `linked` picks the direction; the answer is the whole CodeStudioProjectLinkListResponse, so nothing has to be merged client-side. */
  codeStudioProjectLinkSetRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      workspace_id: csText(payload.workspaceId ?? payload.workspace_id),
      project_id: csText(payload.projectId ?? payload.project_id),
      linked: !!(payload.linked ?? false),
    };
    const body = _wasm.encodeCodeStudioProjectLinkSetRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },

  /** MessageBody::CodeStudioBody(RepoTreeRequest). payload: { workspaceId, projectId, commit, pathPrefix, limit } — structure of a PINNED commit of a linked workspace. `commit` is a resolved object id: a branch name would let the listed code drift from the tested one, and no host path exists on this wire. */
  codeStudioRepoTreeRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const request = {
      workspace_id: csText(payload.workspaceId ?? payload.workspace_id),
      project_id: csText(payload.projectId ?? payload.project_id),
      commit: csText(payload.commit),
      path_prefix: csText(payload.pathPrefix ?? payload.path_prefix),
      limit: Number(payload.limit ?? 500),
    };
    const body = _wasm.encodeCodeStudioRepoTreeRequest(JSON.stringify(request));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
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

/**
 * Code Studio requests address a workspace and, apart from the workspace-level
 * ones, a session. Both ids are read from either casing here so fifty encoders
 * do not repeat the same fallback chain.
 */
function csScope(payload) {
  return {
    workspace_id: csText(payload.workspaceId ?? payload.workspace_id),
    session_id: csText(payload.sessionId ?? payload.session_id),
  };
}

/** Required snake_case string field: absent becomes `fallback`, never `undefined`. */
function csText(value, fallback = '') {
  return value == null ? fallback : String(value);
}

/** Optional field: absent or empty becomes JSON null, which serde reads as None. */
function csOptText(value) {
  return value == null || value === '' ? null : String(value);
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
