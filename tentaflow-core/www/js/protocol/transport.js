// =============================================================================
// File: protocol/transport.js
// Description: Browser-to-daemon transport abstraction. It uses WebTransport
// when the browser supports it and falls back to WebSocket otherwise, while
// exposing the same send/onmessage contract to the upper layers.
// =============================================================================

import { codecReady } from './codec.js';

export const TRANSPORT_WEBTRANSPORT = 'webtransport';
export const TRANSPORT_WEBSOCKET = 'websocket';

/**
 * Creates the best available transport instance. It tries WebTransport on
 * `/wt/api` first and falls back to WebSocket on `/ws/api` if that fails.
 * Both endpoints are served by the daemon on the same HTTPS port.
 */
export async function openTransport(options = {}) {
  await codecReady;
  const {
    jwtToken = null,
    preferred = TRANSPORT_WEBSOCKET,
    webTransportTimeoutMs = 3000,
  } = options;

  const baseUrl = window.location.origin;

  // WebTransport needs an HTTP/3 endpoint on `/wt/api`. The current unified
  // server only exposes HTTPS/1.1 and HTTP/2 with WebSocket upgrade, so
  // WebSocket stays the default until iroh-relay is wired in as an H3 endpoint.
  if (preferred === TRANSPORT_WEBTRANSPORT && typeof window.WebTransport === 'function') {
    try {
      const wt = await Promise.race([
        openWebTransport(baseUrl, jwtToken),
        new Promise((_, reject) =>
          setTimeout(() => reject(new Error('WebTransport timeout')), webTransportTimeoutMs),
        ),
      ]);
      return wt;
    } catch (err) {
      console.debug('[transport] WebTransport unavailable, falling back to WebSocket:', err.message);
    }
  }

  return openWebSocket(baseUrl, jwtToken);
}

/**
 * WebTransport implementation that opens an HTTP/3 connection to `/wt/api`.
 * Each message uses a separate bidirectional stream to keep the transport
 * simple.
 */
async function openWebTransport(baseUrl, jwtToken) {
  const httpsBase = baseUrl.replace(/^http:/, 'https:');
  const url = `${httpsBase}/wt/api${jwtToken ? `?token=${encodeURIComponent(jwtToken)}` : ''}`;
  const wt = new WebTransport(url);
  await wt.ready;

  const listeners = new Set();
  const closeListeners = new Set();
  let closed = false;

  const fireClose = (reason) => {
    if (closed) return;
    closed = true;
    for (const cb of closeListeners) {
      try { cb(reason); } catch (e) { console.error('[transport] close listener threw:', e); }
    }
  };

  // Incoming unidirectional streams carry event and response frames.
  (async () => {
    const reader = wt.incomingUnidirectionalStreams.getReader();
    while (!closed) {
      const { done, value: stream } = await reader.read().catch(() => ({ done: true }));
      if (done) break;
      const body = await readAllFromStream(stream).catch(() => null);
      if (body && listeners.size > 0) {
        for (const cb of listeners) {
          try {
            cb(body);
          } catch (e) {
            console.error('[transport] listener threw:', e);
          }
        }
      }
    }
    fireClose({ code: 0, reason: 'stream ended' });
  })();

  wt.closed.then((info) => {
    fireClose({ code: info?.closeCode ?? 0, reason: info?.reason ?? 'wt closed' });
  }).catch((err) => {
    fireClose({ code: -1, reason: String(err?.message ?? err) });
  });

  return {
    kind: TRANSPORT_WEBTRANSPORT,
    async send(bytes) {
      if (closed) throw new Error('transport closed');
      const writer = (await wt.createUnidirectionalStream()).getWriter();
      await writer.write(bytes);
      await writer.close();
    },
    onMessage(cb) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    onClose(cb) {
      closeListeners.add(cb);
      return () => closeListeners.delete(cb);
    },
    close() {
      if (closed) return;
      closed = true;
      try { wt.close(); } catch { /* ignore */ }
      for (const cb of closeListeners) {
        try { cb({ code: 1000, reason: 'client close' }); } catch { /* ignore */ }
      }
    },
    isOpen() {
      return !closed;
    },
  };
}

async function readAllFromStream(stream) {
  const reader = stream.getReader();
  const chunks = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    total += value.byteLength;
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const c of chunks) {
    out.set(c, offset);
    offset += c.byteLength;
  }
  return out;
}

/**
 * WebSocket implementation for compatibility with the existing server.
 * ALPN `bearer.<token>` in `Sec-WebSocket-Protocol` carries the JWT during the
 * upgrade request.
 */
async function openWebSocket(baseUrl, jwtToken) {
  const wsScheme = baseUrl.startsWith('https') ? 'wss' : 'ws';
  const url = `${wsScheme}://${window.location.host}/ws/api`;
  const protocols = [];
  if (jwtToken) protocols.push(`bearer.${jwtToken}`);

  const ws = protocols.length > 0 ? new WebSocket(url, protocols) : new WebSocket(url);
  ws.binaryType = 'arraybuffer';

  await new Promise((resolve, reject) => {
    ws.onopen = () => resolve();
    ws.onerror = (_e) => reject(new Error('WebSocket connection failed'));
  });

  const listeners = new Set();
  const closeListeners = new Set();
  let localClosed = false;

  ws.onmessage = (evt) => {
    const bytes = new Uint8Array(evt.data);
    for (const cb of listeners) {
      try {
        cb(bytes);
      } catch (e) {
        console.error('[transport] listener threw:', e);
      }
    }
  };

  // Propagate close events to the upper layer so reconnect logic can react.
  ws.onclose = (evt) => {
    const info = {
      code: evt?.code ?? 1006,
      reason: evt?.reason || 'ws closed',
      wasClean: !!evt?.wasClean,
      local: localClosed,
    };
    for (const cb of closeListeners) {
      try { cb(info); } catch (e) { console.error('[transport] close listener threw:', e); }
    }
  };

  // `onerror` before `onopen` rejects the promise. Later errors still lead to
  // `onclose`, so there is no need to duplicate reporting here.
  ws.onerror = (_e) => { /* onclose will emit the details */ };

  return {
    kind: TRANSPORT_WEBSOCKET,
    async send(bytes) {
      if (ws.readyState !== WebSocket.OPEN) throw new Error('ws not open');
      ws.send(bytes);
    },
    onMessage(cb) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    onClose(cb) {
      closeListeners.add(cb);
      return () => closeListeners.delete(cb);
    },
    close() {
      localClosed = true;
      try { ws.close(1000, 'client close'); } catch { /* ignore */ }
    },
    isOpen() {
      return ws.readyState === WebSocket.OPEN;
    },
  };
}
