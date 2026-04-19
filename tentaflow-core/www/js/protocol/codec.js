// =============================================================================
// Plik: codec.js
// Opis: Fasada nad wasm-bindgen glue dla tentaflow-protocol-wasm.
//       Abstrahuje init().then() w pojedyncze `codecReady` Promise i
//       eksportuje typed helpery do budowy binarych frameow WebSocket.
// Przyklad:
//   import { codecReady, encode } from '/js/protocol/codec.js';
//   await codecReady;
//   const frame = encode.nodeListRequest(nextCorrelationId());
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
  /** MessageBody::NodeListRequest — lista nodow mesh. */
  nodeListRequest(correlationId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeNodeListRequest();
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
      body,
    );
  },

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

  /** MessageBody::NodeInfoRequest — szczegoly konkretnego noda (32-byte node_id). */
  nodeInfoRequest(correlationId, nodeId, sequence = 1) {
    assertReady();
    const body = _wasm.encodeNodeInfoRequest(nodeId);
    return _wasm.encodeEnvelopeDirect(
      BigInt(correlationId),
      BigInt(sequence),
      _messageKind.META_HEARTBEAT,
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

  /** MessageBody::ClusterUpdateRequest { cluster_id, name, description } */
  clusterUpdateRequest(correlationId, { clusterId, name, description }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeClusterUpdateRequest(clusterId, name, description ?? null);
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

  /** MessageBody::ServiceStopRequest { serviceId } */
  serviceStopRequest(correlationId, { serviceId }, sequence = 1) {
    assertReady();
    const body = _wasm.encodeServiceStopRequest(serviceId);
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
