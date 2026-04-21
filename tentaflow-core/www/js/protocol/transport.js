// =============================================================================
// Plik: protocol/transport.js
// Opis: Abstrakcja transportu browser↔daemon. WebTransport gdy przegladarka
//       wspiera, WebSocket jako kolejna dostepna droga. Ten sam kontrakt
//       wyjsciowy (send bajtow, onmessage callback) dla wyzszych warstw
//       (binary-ws-client.js wpiete bez zmian). Identity browser-a to
//       Ed25519 NodeId generowany w module WASM (identity.rs), persistowany
//       w localStorage. JWT zostaje jako warstwa session-level auth.
// =============================================================================

import { codecReady } from './codec.js';

export const TRANSPORT_WEBTRANSPORT = 'webtransport';
export const TRANSPORT_WEBSOCKET = 'websocket';

/**
 * Tworzy instancje transportu najlepszego dostepnego typu. Probuje
 * WebTransport pod `/wt/api`, w razie bledu przelacza na WebSocket `/ws/api`.
 * Oba endpoints daemon serwuje pod tym samym portem HTTPS.
 */
export async function openTransport(options = {}) {
  await codecReady;
  const {
    jwtToken = null,
    preferred = TRANSPORT_WEBSOCKET,
    webTransportTimeoutMs = 3000,
  } = options;

  const baseUrl = window.location.origin;

  // WebTransport wymaga serwera H3 pod `/wt/api` — aktualnie unified_server
  // obsluguje tylko HTTPS/1.1+H2 z upgrade do WebSocket. Dopoki iroh-relay
  // nie jest wpiety jako H3 endpoint na tym samym porcie, zostajemy przy WS.
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
      console.debug('[transport] WebTransport niedostepne, uzywam WebSocket:', err.message);
    }
  }

  return openWebSocket(baseUrl, jwtToken);
}

/**
 * Transport WebTransport — otwiera polaczenie HTTP/3 do /wt/api.
 * Daemon hostuje endpoint WebTransport razem z iroh-relay (przez http_server
 * iroh-relay). Kazda wiadomosc = jeden bidi stream do prostoty.
 */
async function openWebTransport(baseUrl, jwtToken) {
  const httpsBase = baseUrl.replace(/^http:/, 'https:');
  const url = `${httpsBase}/wt/api${jwtToken ? `?token=${encodeURIComponent(jwtToken)}` : ''}`;
  const wt = new WebTransport(url);
  await wt.ready;

  const listeners = new Set();
  let closed = false;

  // Incoming unidirectional streams — tutaj serwer wysyla event/response frames.
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
  })();

  wt.closed.then(() => {
    closed = true;
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
    close() {
      closed = true;
      try {
        wt.close();
      } catch {
        // ignore
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
 * Transport WebSocket — sciezka kompatybilna z istniejacym serwerem.
 * ALPN `bearer.<token>` w Sec-WebSocket-Protocol pozwala na JWT przy upgrade.
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
    ws.onerror = (e) => reject(new Error('WebSocket connection failed'));
  });

  const listeners = new Set();

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
    close() {
      try {
        ws.close();
      } catch {
        // ignore
      }
    },
    isOpen() {
      return ws.readyState === WebSocket.OPEN;
    },
  };
}
