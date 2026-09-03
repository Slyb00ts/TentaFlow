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
// Deadline for a whole call through the shim, not just for its round trip. The
// client's own timer starts AFTER the frame is encoded, so a transport that
// stalls while connecting used to leave the caller's promise pending forever —
// a page waiting on a write that will never answer either way. One deadline per
// call, applied here, is what makes "every request settles" true for all of
// them instead of for the ones that got as far as the socket.
const CALL_DEADLINE_MS = 30_000;

/** Rejects `promise` if it has not settled within `timeoutMs`. */
function withDeadline(promise, timeoutMs, what) {
  let timer = null;
  const deadline = new Promise((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`request ${what} timed out after ${timeoutMs}ms`)),
      timeoutMs,
    );
  });
  return Promise.race([promise, deadline]).finally(() => clearTimeout(timer));
}

/**
 * The caller's `{timeoutMs}` options object, read without consuming it —
 * `BinaryWsClient.request` pops the same argument for its own timer, and both
 * timers have to describe the same deadline.
 */
function requestedTimeout(args) {
  const last = args[args.length - 1];
  if (last && typeof last === 'object' && last._isRequestOptions === true) {
    if (typeof last.timeoutMs === 'number') {
      return last.timeoutMs;
    }
    // A forwarded request waits for the mesh round-trip (server side allows
    // 45 s) — the shim deadline must not fire first.
    if (typeof last.targetNodeId === 'string' && last.targetNodeId) {
      return Math.max(CALL_DEADLINE_MS, 50000);
    }
  }
  return CALL_DEADLINE_MS;
}

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
    onUpdateAvailable: (info) => emit({ type: 'update-available', info }),
  });
}

/**
 * Otwiera WS natychmiast (przed logowaniem). Anonymous WS — serwer akceptuje
 * bez JWT i pozwala tylko na authLoginRequest + schema + heartbeat. Po udanym
 * loginie `setJwt()` zamyka i ponownie otwiera z JWT.
 */
export async function initTransport() {
  await codecReady;
  try {
    return await getClient();
  } catch (e) {
    if (!_client) {
      _client = buildClient();
    }
    // connect() samo zaplanuje reconnect. Overlay juz dostanie notify.
    console.warn('[api-binary] initial connect failed:', e?.message);
    return _client;
  }
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

function dispatch(kind, ...args) {
  const timeoutMs = requestedTimeout(args);
  const call = (async () => {
    const client = await getClient();
    const result = await client.request(kind, ...args);
    if (result.envelope.isError || result.body.variant === 'Error') {
      const err = new Error(result.body.message ?? `protocol error in ${kind}`);
      err.code = result.body.code;
      throw err;
    }
    return result.body;
  })();
  return withDeadline(call, timeoutMs, kind);
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

  async action(kind, payload, options) {
    const opts = { _isRequestOptions: true };
    let hasOpts = false;
    if (options && typeof options.timeoutMs === 'number') {
      opts.timeoutMs = options.timeoutMs;
      hasOpts = true;
    }
    // targetNodeId adresuje request do innego wezla floty (Routing::Forward).
    if (options && typeof options.targetNodeId === 'string' && options.targetNodeId) {
      opts.targetNodeId = options.targetNodeId;
      hasOpts = true;
    }
    return hasOpts ? dispatch(kind, payload, opts) : dispatch(kind, payload);
  },

  async subscribe(kind, payload, { onChunk, onEnd, onError } = {}) {
    // Opening a stream waits on the same transport a request does, so it gets
    // the same deadline; without it a subscribe on a dead socket hangs the
    // screen that awaited it.
    const client = await withDeadline(getClient(), CALL_DEADLINE_MS, kind);
    const correlationId = client.nextCorrelationId();
    const sequence = client.takeSequence();
    const codec = await import('./codec.js');
    if (typeof codec.encode[kind] !== 'function') {
      throw new Error(`unknown request kind '${kind}': the codec has no encoder for it`);
    }
    const frame = codec.encode[kind](correlationId, payload, sequence);

    const removeListener = client.subscribe(correlationId, ({ envelope, body }) => {
      if (envelope.isError) {
        onError?.(body);
      } else if (envelope.isStreamEnd) {
        onEnd?.(body);
      } else {
        onChunk?.(body);
      }
    });

    client._send(frame);

    // The returned unsubscribe drops the local listener AND emits a
    // StreamCloseRequest on the SAME correlation id as this subscribe. The
    // server's stream-close handler cancels the subscription by the close
    // frame's correlation id (dispatch/stream.rs), so the close MUST reuse the
    // original id — going through ApiBinary.action would mint a fresh id and the
    // server would keep the subscription (slot + frame pump) alive until socket
    // EOF. Best-effort: a stream payload carries `streamId`, but the server keys
    // cancellation purely on correlation id, so an empty id here is fine.
    let closed = false;
    return () => {
      removeListener();
      if (closed) return;
      closed = true;
      try {
        const closeSeq = client.takeSequence();
        const closeFrame = codec.encode.streamCloseRequest(
          correlationId,
          { streamId: payload?.streamId ?? payload?.stream_id ?? '' },
          closeSeq,
        );
        client._send(closeFrame);
      } catch (e) {
        console.warn('[api-binary] stream close emit failed:', e?.message ?? e);
      }
    };
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
    _connectingPromise = null;
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
    _connectingPromise = null;
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
