// =============================================================================
// Plik: binary-ws-client.js
// Opis: Binary WebSocket client dla nowego protokolu rkyv. Wrappuje WebSocket
//       z bytecheck walidacja, correlation_id trackingiem, reconnect z drain
//       kolejki wysylkowej i handshake schema version.
// Przyklad:
//   const client = new BinaryWsClient('/ws/api');
//   await client.connect();
//   const response = await client.request('nodeListRequest');
//   for (const node of response.body.nodes) console.log(node.displayName);
// =============================================================================

import { codecReady, encode, decodeFrame, schemaVersion, makeCorrelationIdGenerator } from './codec.js';
import { openTransport, TRANSPORT_WEBTRANSPORT, TRANSPORT_WEBSOCKET } from './transport.js';

const DEFAULT_TIMEOUT_MS = 30_000;
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30_000;

export class BinaryWsClient {
  /**
   * @param {string} url — WebSocket URL (`ws://` / `wss://`)
   * @param {object} opts
   *   @param {number} [opts.heartbeatIntervalMs=15000]
   *   @param {number} [opts.requestTimeoutMs=30000]
   *   @param {function} [opts.onOpen]
   *   @param {function} [opts.onClose]
   *   @param {function} [opts.onProtocolError] — error frame (server-initiated)
   *   @param {function} [opts.onUnsolicited] — eventy serwer→klient bez request matchu
   */
  constructor(url, opts = {}) {
    this.url = url;
    this.ws = null;
    this.connected = false;
    this.nextCorrelationId = null;
    this.pending = new Map();
    this.subscribers = new Map();
    this.heartbeatTimer = null;
    this.reconnectAttempt = 0;
    this.closed = false;
    this.outbox = [];
    // Sequence per connection — server odrzuca <= last_seen, wiec MUSI rosnac.
    this.nextSequence = 1n;

    this.jwtToken = opts.jwtToken ?? null;
    this.heartbeatIntervalMs = opts.heartbeatIntervalMs ?? 15_000;
    this.requestTimeoutMs = opts.requestTimeoutMs ?? DEFAULT_TIMEOUT_MS;
    this.onOpen = opts.onOpen ?? noop;
    this.onClose = opts.onClose ?? noop;
    this.onProtocolError = opts.onProtocolError ?? noop;
    // P2c FIX: lista listenerow dla unsolicited frame (kazda screen moze
    // dodac swoj). Stare onUnsolicited (single) zachowane jako shortcut.
    this._unsolicitedListeners = [];
    if (opts.onUnsolicited) this._unsolicitedListeners.push(opts.onUnsolicited);
  }

  /**
   * Dodaje listener dla unsolicited frame (server-push events bez request match).
   * Zwraca unsubscribe callback.
   */
  addUnsolicitedListener(listener) {
    this._unsolicitedListeners.push(listener);
    return () => {
      const idx = this._unsolicitedListeners.indexOf(listener);
      if (idx >= 0) this._unsolicitedListeners.splice(idx, 1);
    };
  }

  /**
   * Backward compat: ustawia jedyny listener (zachowuje stary onUnsolicited semantyke).
   * Lepiej uzywac addUnsolicitedListener dla composition.
   */
  set onUnsolicited(listener) {
    this._unsolicitedListeners = listener ? [listener] : [];
  }
  get onUnsolicited() {
    return this._unsolicitedListeners[0] ?? noop;
  }

  /**
   * Laczy i wykonuje handshake schema version. Rejectuje gdy serwer odrzuci.
   */
  async connect() {
    await codecReady;
    if (!this.nextCorrelationId) {
      this.nextCorrelationId = makeCorrelationIdGenerator();
    }

    try {
      this.transport = await openTransport({ jwtToken: this.jwtToken });
      this.connected = true;
      this.reconnectAttempt = 0;
      this._transportUnsub = this.transport.onMessage((bytes) => this._handleBytes(bytes));
      console.info(`[ws] transport: ${this.transport.kind}`);
      await this._handshake();
      this._startHeartbeat();
      this._drainOutbox();
      this.onOpen();
    } catch (err) {
      this.connected = false;
      if (this.transport) this.transport.close();
      this.transport = null;
      throw err;
    }
  }

  /**
   * Zamyka klient. Po close() nie bedzie auto-reconnectu.
   */
  close() {
    this.closed = true;
    this._stopHeartbeat();
    if (this._transportUnsub) this._transportUnsub();
    if (this.transport) this.transport.close();
    this.transport = null;
  }

  /**
   * Pobiera kolejny sequence number (BigInt). Server wymaga monotonicznie
   * rosnacych sequences w obrebie connection.
   */
  takeSequence() {
    const seq = this.nextSequence;
    this.nextSequence = this.nextSequence + 1n;
    return seq;
  }

  /**
   * Wysyla request i czeka na odpowiedz po correlation_id.
   * @param {string} kind — klucz z `encode` (np. `'nodeListRequest'`)
   * @param {...any} args — argumenty przekazywane do `encode[kind]` (po correlation_id, sequence)
   * @returns {Promise<{envelope, body}>}
   */
  request(kind, ...args) {
    const correlationId = this.nextCorrelationId();
    const sequence = this.takeSequence();
    const frame = encode[kind](correlationId, ...args, sequence);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(correlationId.toString());
        reject(new Error(`request ${kind} timed out after ${this.requestTimeoutMs}ms`));
      }, this.requestTimeoutMs);

      this.pending.set(correlationId.toString(), {
        resolve: (result) => {
          clearTimeout(timer);
          resolve(result);
        },
        reject: (err) => {
          clearTimeout(timer);
          reject(err);
        },
      });
      this._send(frame);
    });
  }

  /**
   * Subskrypcja na stream po correlation_id. Callback wolany dla kazdego chunka,
   * ostatni wolany z is_stream_end=true.
   */
  subscribe(correlationId, onChunk) {
    this.subscribers.set(correlationId.toString(), onChunk);
    return () => this.subscribers.delete(correlationId.toString());
  }

  /**
   * Wysyla raw Uint8Array. Gdy zakolejkowany w outbox, drain po reconnect.
   */
  _send(bytes) {
    if (this.connected && this.transport?.isOpen()) {
      this.transport.send(bytes).catch((err) => {
        console.error('[ws] send error:', err);
      });
    } else {
      this.outbox.push(bytes);
    }
  }

  _drainOutbox() {
    while (this.outbox.length > 0 && this.connected) {
      const frame = this.outbox.shift();
      this.transport.send(frame).catch((err) => {
        console.error('[ws] drain send error:', err);
      });
    }
  }

  async _handshake() {
    const correlationId = this.nextCorrelationId();
    const sequence = this.takeSequence();
    const frame = encode.metaSchemaVersionCheck(correlationId, schemaVersion(), sequence);
    const resultPromise = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(correlationId.toString());
        reject(new Error('handshake timeout'));
      }, 5000);
      this.pending.set(correlationId.toString(), {
        resolve: (r) => {
          clearTimeout(timer);
          resolve(r);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
    });
    await this.transport.send(frame);
    const { body } = await resultPromise;
    if (body.variant !== 'MetaSchemaVersionAck' || !body.accepted) {
      throw new Error(
        `schema version mismatch: client=${schemaVersion()} server=${body.serverVersion}`,
      );
    }
  }

  _handleBytes(bytes) {
    let decoded;
    try {
      decoded = decodeFrame(bytes);
    } catch (err) {
      console.error('[ws] malformed frame:', err);
      return;
    }

    const { envelope, body } = decoded;
    const correlationKey = envelope.correlationId.toString();

    if (envelope.isError) {
      const pending = this.pending.get(correlationKey);
      if (pending) {
        this.pending.delete(correlationKey);
        pending.reject(
          new Error(`protocol error ${body.code ?? 'Unknown'}: ${body.message ?? ''}`),
        );
      } else {
        this.onProtocolError(body);
      }
      return;
    }

    if (envelope.isStreamChunk || envelope.isStreamEnd) {
      const sub = this.subscribers.get(correlationKey);
      if (sub) sub({ envelope, body });
      if (envelope.isStreamEnd) this.subscribers.delete(correlationKey);
      return;
    }

    const pending = this.pending.get(correlationKey);
    if (pending) {
      this.pending.delete(correlationKey);
      pending.resolve({ envelope, body });
      return;
    }

    // P2c FIX: dispatch do wszystkich listenerow (multiple screens).
    for (const listener of this._unsolicitedListeners) {
      try {
        listener({ envelope, body });
      } catch (err) {
        console.error('[ws] unsolicited listener threw:', err);
      }
    }
  }

  _startHeartbeat() {
    if (this.heartbeatIntervalMs <= 0) return;
    this.heartbeatTimer = setInterval(() => {
      if (!this.connected || !this.transport?.isOpen()) return;
      const correlationId = this.nextCorrelationId();
      const sequence = this.takeSequence();
      const frame = encode.metaHeartbeat(correlationId, Math.floor(Date.now() / 1000), sequence);
      this.transport.send(frame).catch((err) => console.error('[ws] heartbeat send:', err));
    }, this.heartbeatIntervalMs);
  }

  _stopHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
  }

  _rejectAllPending(err) {
    for (const { reject } of this.pending.values()) reject(err);
    this.pending.clear();
  }

  _scheduleReconnect() {
    const delay = Math.min(
      RECONNECT_BASE_MS * 2 ** this.reconnectAttempt,
      RECONNECT_MAX_MS,
    );
    this.reconnectAttempt += 1;
    setTimeout(() => {
      if (!this.closed) this.connect().catch(() => {});
    }, delay);
  }
}

function noop() {}
