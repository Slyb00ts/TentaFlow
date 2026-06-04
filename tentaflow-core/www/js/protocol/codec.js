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

  /** MessageBody::ApiKeyCreateRequest { name, scopes: string[] } */
  apiKeyCreateRequest(correlationId, { name, scopes = [] }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeApiKeyCreateRequest(name, scopes);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
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
  chatStreamRequest(correlationId, { modelId, userMessage }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeChatStreamRequestSimple(modelId, userMessage);
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

  /** MessageBody::DeploymentBody(ReqRedeploy { serviceId, forceIfActiveSessions }) */
  serviceRedeployRequest(correlationId, { serviceId, forceIfActiveSessions = false }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceRedeployRequest(Number(serviceId), !!forceIfActiveSessions);
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
  // Notes
  // -------------------------------------------------------------------------

  /** NotesRequest::List — no payload. */
  notesListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeNotesListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** NotesRequest::Detail { noteId } */
  noteDetailRequest(correlationId, { noteId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeNoteDetailRequest(Number(noteId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** NotesRequest::Create { title, body } */
  noteCreateRequest(correlationId, { title, body }, sequence = 1) {
    assertReady();
    const payload = _wasm.encodeNoteCreateRequest(title ?? '', body ?? '');
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      payload,
    );
  },

  /** NotesRequest::Update { noteId, title, body } */
  noteUpdateRequest(correlationId, { noteId, title, body }, sequence = 1) {
    assertReady();
    const payload = _wasm.encodeNoteUpdateRequest(Number(noteId), title ?? '', body ?? '');
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      payload,
    );
  },

  /** NotesRequest::SetPinned { noteId, pinned } */
  noteSetPinnedRequest(correlationId, { noteId, pinned }, sequence = 1) {
    assertReady();
    const payload = _wasm.encodeNoteSetPinnedRequest(Number(noteId), !!pinned);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      payload,
    );
  },

  /** NotesRequest::Delete { noteId } */
  noteDeleteRequest(correlationId, { noteId }, sequence = 1) {
    assertReady();
    const payload = _wasm.encodeNoteDeleteRequest(Number(noteId));
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      payload,
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
    { packageId, version, displayName },
    sequence = 1,
  ) {
    assertReady();
    const body = _wasm.encodeAddonInstanceInstallRequest(packageId, version, displayName);
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
   *  correlation_id. Payload: { streamId }. */
  streamSubscribeRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const streamId = String(payload?.streamId ?? payload?.stream_id ?? '');
    const body = _wasm.encodeStreamSubscribeRequest(streamId);
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
