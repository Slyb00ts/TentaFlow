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

  /** MessageBody::ModelsUnifiedListRequest — unikalne modele ze wszystkich nodow mesh. */
  modelsUnifiedListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeModelsUnifiedListRequest();
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
    const body = _wasm.encodeFlowDetailRequest(flowId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::FlowCreateRequest { name, description, graphJson } */
  flowCreateRequest(correlationId, { name, description, graphJson }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowCreateRequest(name, description ?? null, graphJson);
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
    const body = _wasm.encodeFlowDeleteRequest(flowId);
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
    const body = _wasm.encodeFlowExecutionsListRequest(flowId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

  /** MessageBody::FlowUpdateRequest — partial update flow. */
  flowUpdateRequest(correlationId, payload, sequence = 1) {
    assertReady();
    const body = _wasm.encodeFlowUpdateRequest(
      String(payload.flowId),
      payload.name ?? null,
      payload.description ?? null,
      payload.flowJson ?? null,
      payload.status ?? null,
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
    const body = _wasm.encodeNetworkConfigUpdateRequest(
      bindMode,
      bindIpv4,
      hideDocker,
      hideLinkLocal,
      hideLoopback,
      hideCgnat,
      preferSameSubnet,
      irohRelayUrl,
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
      payload.defaultGroupId == null ? undefined : Number(payload.defaultGroupId),
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
      payload.userId ?? null,
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

  /**
   * MessageBody::AuditLogExportRequest — Admin. Eksport CSV z filtrami
   * (max 100_000 wierszy). payload: { userId?, addonId?, action?, fromDate?, toDate?, search? }
   */
  auditLogExportRequest(correlationId, payload = {}, sequence = 1) {
    assertReady();
    const body = _wasm.encodeAuditLogExportRequest(
      payload.userId ?? null,
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
      Number(payload.groupId ?? 0),
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
      Number(payload.subjectId ?? 0),
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
    const userId = payload.userId == null ? null : Number(payload.userId);
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
    const body = _wasm.encodeIamGetUserRequest(Number(userId));
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
      Number(p.userId), String(p.displayName ?? ''), String(p.email ?? ''),
      !!p.isActive, String(p.role ?? 'user'),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamDeleteUserRequest(correlationId, { userId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamDeleteUserRequest(Number(userId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamSetUserGroupsRequest(correlationId, p, sequence = 1) {
    assertReady();
    const csv = Array.isArray(p.groupIds) ? p.groupIds.join(',') : String(p.groupIds ?? '');
    const body = _wasm.encodeIamSetUserGroupsRequest(Number(p.userId), csv);
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamResetUserPasswordRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamResetUserPasswordRequest(Number(p.userId), String(p.newPassword ?? ''));
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
    const body = _wasm.encodeIamUpdateGroupRequest(Number(p.groupId), String(p.name ?? ''), String(p.description ?? ''));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamDeleteGroupRequest(correlationId, { groupId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamDeleteGroupRequest(Number(groupId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamGroupMembersRequest(correlationId, { groupId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamGroupMembersRequest(Number(groupId));
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamSetPermissionRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamSetPermissionRequest(
      String(p.resourceType), String(p.resourceId),
      String(p.subjectType), Number(p.subjectId), String(p.accessLevel),
    );
    return _wasm.encodeEnvelopeDirect(BigInt(correlationId), BigInt(sequence), _messageKind.META_HEARTBEAT, body);
  },
  iamClearPermissionRequest(correlationId, p, sequence = 1) {
    assertReady();
    const body = _wasm.encodeIamClearPermissionRequest(
      String(p.resourceType), String(p.resourceId),
      String(p.subjectType), Number(p.subjectId),
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
    const body = _wasm.encodeIamListPermsForSubjectRequest(String(p.subjectType), Number(p.subjectId));
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
