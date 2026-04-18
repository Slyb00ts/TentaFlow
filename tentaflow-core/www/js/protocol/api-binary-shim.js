// =============================================================================
// Plik: protocol/api-binary-shim.js
// Opis: Cienki shim dispatchu przez binary WS. Zarządza pojedynczym shared
//       BinaryWsClient instance per page, JWT auth, auto-reconnect.
// =============================================================================

import { codecReady } from './codec.js';
import { BinaryWsClient } from './binary-ws-client.js';

const JWT_STORAGE_KEY = 'tentaflow_jwt';

let _client = null;
let _connectingPromise = null;

async function getClient() {
  if (_client && _client.connected) return _client;
  if (_connectingPromise) return _connectingPromise;

  _connectingPromise = (async () => {
    await codecReady;
    const wsScheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${wsScheme}//${window.location.host}/ws/api`;
    const jwtToken = localStorage.getItem(JWT_STORAGE_KEY);

    _client = new BinaryWsClient(url, {
      jwtToken,
      onClose: () => {
        _client = null;
      },
    });
    await _client.connect();
    _connectingPromise = null;
    return _client;
  })();

  return _connectingPromise;
}

async function dispatch(kind, ...args) {
  const client = await getClient();
  const result = await client.request(kind, ...args);
  if (result.envelope.isError || result.body.variant === 'Error') {
    const err = new Error(result.body.message ?? `protocol error in ${kind}`);
    err.code = result.body.code;
    throw err;
  }
  return result.body;
}

export const ApiBinary = {
  async list(kind, options = {}) {
    const body = await dispatch(kind);
    const arrayKey = options.arrayKey ?? guessArrayKey(body);
    return body[arrayKey] ?? [];
  },

  async one(kind, ...args) {
    return dispatch(kind, ...args);
  },

  async action(kind, payload) {
    return dispatch(kind, payload);
  },

  async subscribe(kind, payload, { onChunk, onEnd, onError } = {}) {
    const client = await getClient();
    const correlationId = client.nextCorrelationId();
    const codec = await import('./codec.js');
    const frame = codec.encode[kind](correlationId, payload);

    const unsubscribe = client.subscribe(correlationId, ({ envelope, body }) => {
      if (envelope.isError) {
        onError?.(body);
      } else if (envelope.isStreamEnd) {
        onEnd?.(body);
      } else {
        onChunk?.(body);
      }
    });

    client._send(frame);
    return unsubscribe;
  },

  setJwt(token) {
    if (token) {
      localStorage.setItem(JWT_STORAGE_KEY, token);
    } else {
      localStorage.removeItem(JWT_STORAGE_KEY);
    }
    if (_client) {
      _client.close();
      _client = null;
    }
  },

  getJwt() {
    return localStorage.getItem(JWT_STORAGE_KEY);
  },

  hasJwt() {
    return !!localStorage.getItem(JWT_STORAGE_KEY);
  },

  async client() {
    return getClient();
  },

  clearSession() {
    localStorage.removeItem(JWT_STORAGE_KEY);
    if (_client) {
      _client.close();
      _client = null;
    }
  },
};

function guessArrayKey(body) {
  for (const k of Object.keys(body)) {
    if (Array.isArray(body[k])) return k;
  }
  return null;
}
