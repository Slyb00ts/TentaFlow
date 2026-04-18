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
