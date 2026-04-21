// =============================================================================
// Plik: protocol/api-binary-shim.js
// Opis: Cienki shim ApiClient-podobny ale dispatchuje przez binary WS zamiast
//       fetch(). Pozwala migrowac istniejace ekrany jedna zmiana importu:
//
//         // Przed:
//         keys = await ApiClient.get('/api/apikeys');
//         // Po:
//         keys = await ApiBinary.list('apiKeyListRequest');
//
//       Pelne mapowanie GUI -> binary variants ponizej (uzupelniane przy
//       migracji kolejnych ekranow).
// =============================================================================

import { codecReady } from './codec.js';
import { BinaryWsClient } from './binary-ws-client.js';

let _client = null;

async function getClient() {
  if (_client && _client.connected) return _client;
  await codecReady;

  const wsScheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const url = `${wsScheme}//${window.location.host}/ws/api`;

  // JWT token z localStorage do Sec-WebSocket-Protocol bearer.<token>
  const token = localStorage.getItem('jwt');
  if (token) {
    _client = new BinaryWsClient(url, {
      // BinaryWsClient nie wystawia bezposrednio subprotocols na razie —
      // protokol bearer.<token> przekazuje sie przez global WebSocket factory
      // (TODO: dodac opcje do BinaryWsClient po stabilizacji). Dla bootstrap
      // session dispatched przez extract_ws_user_id na serwerze gdy token
      // przesylany standardowo.
    });
  } else {
    _client = new BinaryWsClient(url);
  }

  await _client.connect();
  return _client;
}

/**
 * Wykonuje request przez binary WS i zwraca decoded body wariantu odpowiedzi.
 * Rzuca Error gdy ack/response = Error (mapped na throw zeby pasowac do fetch
 * wzorca uzywanego przez istniejace ekrany).
 */
async function dispatch(kind, ...args) {
  const client = await getClient();
  const result = await client.request(kind, ...args);
  if (result.envelope.isError || result.body.variant === 'Error') {
    throw new Error(result.body.message ?? `protocol error in ${kind}`);
  }
  return result.body;
}

export const ApiBinary = {
  /**
   * Wywoluje "list"-owy variant (request bez argumentow, zwraca tablicowy
   * response). Mapuje request kind -> response field name.
   */
  async list(kind, options = {}) {
    const body = await dispatch(kind);
    // Konwencja: response body ma jedno pole tablicowe. Zwracamy je.
    const arrayKey = options.arrayKey ?? guessArrayKey(body);
    return body[arrayKey] ?? [];
  },

  /**
   * Wywoluje request, zwraca caly body (kiedy response nie jest lista).
   */
  async one(kind, ...args) {
    return dispatch(kind, ...args);
  },

  /**
   * Wykonuje action (W-CREATE / W-UPDATE / W-DELETE / W-ACTION); zwraca body.
   */
  async action(kind, payload) {
    return dispatch(kind, payload);
  },

  /**
   * Subscribe do streama (R-STREAM). Zwraca unsubscribe callback.
   * onChunk wolane dla kazdego chunka, onEnd na koncu.
   */
  async subscribe(kind, payload, { onChunk, onEnd, onError } = {}) {
    const client = await getClient();
    const correlationId = client.nextCorrelationId();
    const frame = (await import('./codec.js')).encode[kind](correlationId, payload);

    const unsubscribe = client.subscribe(correlationId, ({ envelope, body }) => {
      if (envelope.isError) {
        onError?.(body);
      } else if (envelope.isStreamEnd) {
        onEnd?.(body);
      } else {
        onChunk?.(body);
      }
    });

    // Wyslij request
    client._send(frame);
    return unsubscribe;
  },

  /** Zwroc shared client (na zewnatrz: do bezposredniego dostepu np. heartbeat). */
  async client() {
    return getClient();
  },
};

function guessArrayKey(body) {
  // Konwencja: jezeli jest tylko jedno pole tablicowe, zwroc je.
  for (const k of Object.keys(body)) {
    if (Array.isArray(body[k])) return k;
  }
  return null;
}
