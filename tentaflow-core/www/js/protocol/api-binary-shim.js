// =============================================================================
// Plik: protocol/api-binary-shim.js
// Opis: Cienki shim dispatchu przez binary WS. Pojedynczy shared BinaryWsClient
//       per-page. WS otwiera sie natychmiast po `init()` (nawet przed login) —
//       anonymous WS. Reconnect wbudowany w klienta; overlay dostaje notify
//       przez callbacki (onDisconnected/onReconnectScheduled/onReconnectAttempt).
// =============================================================================

import { codecReady } from './codec.js';
import { BinaryWsClient } from './binary-ws-client.js';

const JWT_STORAGE_KEY = 'tentaflow_jwt';

let _client = null;
let _connectingPromise = null;
let _lifecycleListeners = new Set();

/** Emit lifecycle event do overlay + innych subskrybentow. */
function emit(event) {
  for (const cb of _lifecycleListeners) {
    try { cb(event); } catch (e) { console.error('[api-binary] listener threw:', e); }
  }
}

function buildClient() {
  const wsScheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const url = `${wsScheme}//${window.location.host}/ws/api`;
  const jwtToken = localStorage.getItem(JWT_STORAGE_KEY);

  return new BinaryWsClient(url, {
    jwtToken,
    onOpen: () => emit({ type: 'open' }),
    onDisconnected: (info) => emit({ type: 'disconnected', info }),
    onReconnectScheduled: (info) => emit({ type: 'reconnect-scheduled', info }),
    onReconnectAttempt: (info) => emit({ type: 'reconnect-attempt', info }),
    onClose: (info) => emit({ type: 'close', info }),
    onProtocolError: (err) => emit({ type: 'protocol-error', err }),
  });
}

/**
 * Otwiera WS natychmiast (przed logowaniem). Anonymous WS — serwer akceptuje
 * bez JWT i pozwala tylko na authLoginRequest + schema + heartbeat. Po udanym
 * loginie `setJwt()` zamyka i ponownie otwiera z JWT.
 */
export async function initTransport() {
  if (_client) return _client;
  await codecReady;
  _client = buildClient();
  try {
    await _client.connect();
  } catch (e) {
    // connect() sam zaplanuje reconnect. Overlay juz dostanie notify.
    console.warn('[api-binary] initial connect failed:', e?.message);
  }
  return _client;
}

async function getClient() {
  if (_client && _client.connected) return _client;
  if (_connectingPromise) return _connectingPromise;

  // Jesli backoff pending (client zyje, ma zaplanowany reconnect) — NIE wolno
  // wolac connect() recznie, bo kazdy throw emituje onDisconnected i zasmieca
  // log. Poczekaj az timer odpali reconnect i state wroci do connected albo
  // rzuc wiedzialnym bledem zeby dispatch mogl skrocic timeout.
  if (_client && !_client.connected && _client._reconnectTimer) {
    throw new Error('offline: reconnect in progress');
  }

  _connectingPromise = (async () => {
    await codecReady;
    if (!_client) {
      _client = buildClient();
    }
    if (!_client.connected) {
      await _client.connect();
    }
    _connectingPromise = null;
    return _client;
  })();

  try {
    return await _connectingPromise;
  } finally {
    _connectingPromise = null;
  }
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
    const sequence = client.takeSequence();
    const codec = await import('./codec.js');
    const frame = codec.encode[kind](correlationId, payload, sequence);

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

  /**
   * Ustawia JWT po udanym loginie. Zamyka anonimowe WS i otwiera nowe z bearer.
   */
  async setJwt(token) {
    if (token) {
      localStorage.setItem(JWT_STORAGE_KEY, token);
    } else {
      localStorage.removeItem(JWT_STORAGE_KEY);
    }
    if (_client) {
      _client.close();
      _client = null;
    }
    // Otworz nowy client z (lub bez) JWT.
    await initTransport();
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
    // Anonimowe WS otworzy sie przy nastepnym request lub initTransport().
  },

  /** Recznie wymus reconnect (np. z overlay button "Spróbuj teraz"). */
  reconnectNow() {
    if (_client) _client.reconnectNow();
    else initTransport();
  },

  /** Subscribe do lifecycle events (open/disconnected/reconnect-*). */
  onLifecycle(cb) {
    _lifecycleListeners.add(cb);
    return () => _lifecycleListeners.delete(cb);
  },

  /** Synchronous check — czy aktualnie polaczony. */
  isConnected() {
    return !!(_client && _client.connected);
  },
};

function guessArrayKey(body) {
  for (const k of Object.keys(body)) {
    if (Array.isArray(body[k])) return k;
  }
  return null;
}
