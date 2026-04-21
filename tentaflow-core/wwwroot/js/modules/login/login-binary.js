// =============================================================================
// Plik: modules/login/login-binary.js
// Opis: Demo login flow przez binary WS protocol (Task #37 phase 1 — proof).
//       Zastepuje `fetch('/api/auth/login', {...})` przez binary Envelope z
//       MessageBody::AuthLoginRequestBody. Reszta ekranow (25 total) pojdzie
//       po tym samym wzorcu.
// Przyklad:
//   import { loginViaBinary } from './login-binary.js';
//   const result = await loginViaBinary(username, password);
//   if (result.ok) localStorage.setItem('jwt', result.jwt);
// =============================================================================

import { codecReady } from '/js/protocol/codec.js';
import { BinaryWsClient } from '/js/protocol/binary-ws-client.js';

let _client = null;

/**
 * Zwraca wspoldzielony klient WS dla calej aplikacji. Lazy init.
 * Przed handshake nie ma sesji — login endpoint jest Anonymous w policy table.
 */
async function getClient() {
  if (_client && _client.connected) return _client;
  await codecReady;
  // Dla bootstrap nie mamy JWT jeszcze — serwer akceptuje handshake jako
  // Anonymous gdy Sec-WebSocket-Protocol subprotocol brak bearer token.
  // TODO: po zalogowaniu stworz drugiego klienta z bearerowym tokenem.
  _client = new BinaryWsClient('wss://localhost:8090/ws/api', {
    onClose: () => {
      console.log('[login-binary] WS closed');
      _client = null;
    },
  });
  await _client.connect();
  return _client;
}

/**
 * Wykonuje login przez binary protocol.
 * @param {string} username
 * @param {string} password
 * @returns {Promise<{ok: boolean, jwt?: string, userId?: Uint8Array, role?: string, error?: string}>}
 */
export async function loginViaBinary(username, password) {
  try {
    const client = await getClient();
    const response = await client.request('authLoginRequest', { username, password });

    if (response.body.variant !== 'AuthLoginResponse') {
      return {
        ok: false,
        error: `unexpected variant ${response.body.variant}`,
      };
    }

    return {
      ok: true,
      jwt: response.body.jwt,
      userId: response.body.userId,
      role: response.body.role,
    };
  } catch (err) {
    return { ok: false, error: err.message ?? String(err) };
  }
}

/**
 * Wykonuje logout — zamyka WS connection.
 */
export function logoutBinary() {
  if (_client) {
    _client.close();
    _client = null;
  }
}
