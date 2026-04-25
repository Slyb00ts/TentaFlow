// =============================================================================
// File: modules/meeting/vnc-transport.js
// Description: noVNC "raw channel" adapter that tunnels RFB bytes through the
//              dashboard binary WebSocket (ApiBinary) instead of a direct
//              WebSocket to a noVNC proxy. Plugs into RFB via the second
//              constructor argument (non-string = preopened channel path),
//              which makes Websock.attach(this) accept us as a drop-in.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// Status codes produced by VncTunnelOpenResponse. Kept as plain strings to
// mirror the backend contract; consumer code matches on them for UI toasts.
export const VNC_STATUS_OK = 'ok';
export const VNC_STATUS_NOT_FOUND = 'not_found';
export const VNC_STATUS_FORBIDDEN = 'forbidden';
export const VNC_STATUS_NO_PORT = 'no_port';
export const VNC_STATUS_REMOTE_NODE = 'remote_node';
export const VNC_STATUS_FAILED = 'failed';

/**
 * Matches the shape noVNC's Websock.attach() validates:
 * `send`, `close`, `binaryType`, `onerror`, `onmessage`, `onopen`, `protocol`,
 * `readyState`. Event properties are settable fields — Websock.attach()
 * overwrites onmessage/onopen/onerror/onclose with its own handlers.
 */
export class VncApiBinaryTransport {
  constructor(sessionId) {
    if (!Number.isFinite(sessionId)) {
      throw new Error('VncApiBinaryTransport: sessionId must be a finite number');
    }
    this._sessionId = sessionId;
    this._tunnelId = null;
    this._unsubscribe = null;
    this._opened = false;
    this._closed = false;
    // Mirrors native WebSocket readyState numeric enum so noVNC's ReadyStates
    // check resolves to "connecting" | "open" | "closing" | "closed".
    this._readyState = WebSocket.CONNECTING;

    // rawChannelProps from noVNC — must be own properties on the instance.
    this.binaryType = 'arraybuffer';
    this.protocol = '';
    this.onmessage = null;
    this.onopen = null;
    this.onerror = null;
    this.onclose = null;
  }

  get readyState() {
    return this._readyState;
  }

  /**
   * Mounts the tunnel. `onStatus({status, error})` fires exactly once with the
   * first OpenResponse so the caller can render UX for non-ok statuses before
   * Websock.attach() hijacks our `onopen`.
   */
  async start(onStatus) {
    try {
      this._unsubscribe = await ApiBinary.subscribe(
        'vncTunnelOpenRequest',
        { sessionId: this._sessionId },
        {
          onChunk: (body) => this._handleChunk(body, onStatus),
          onEnd: () => this._handleStreamEnd('server-end'),
          onError: (err) => this._handleTransportError(err, onStatus),
        },
      );
    } catch (err) {
      this._readyState = WebSocket.CLOSED;
      onStatus?.({ status: VNC_STATUS_FAILED, error: err?.message || 'subscribe failed' });
      if (typeof this.onerror === 'function') this.onerror(err);
      if (typeof this.onclose === 'function') this.onclose({ code: 1006, reason: 'subscribe failed' });
    }
  }

  _handleChunk(body, onStatus) {
    if (this._closed) return;
    const variant = body?.variant;
    if (variant === 'VncTunnelOpenResponse') {
      const status = body.status;
      if (status === VNC_STATUS_OK) {
        this._tunnelId = body.tunnelId;
        this._opened = true;
        this._readyState = WebSocket.OPEN;
        onStatus?.({ status: VNC_STATUS_OK });
        if (typeof this.onopen === 'function') this.onopen({ type: 'open' });
      } else {
        onStatus?.({ status, error: body.error || '' });
        this._readyState = WebSocket.CLOSED;
        // Tear the subscription down; backend already ended its side but we
        // must not leak the correlation-id entry in the client.
        this._teardown('open-failed');
        if (typeof this.onclose === 'function') {
          this.onclose({ code: 1002, reason: status });
        }
      }
      return;
    }
    if (variant === 'VncTunnelChunk') {
      if (!this._opened) return;
      const bytes = body.bytes;
      if (!(bytes instanceof Uint8Array) || bytes.length === 0) return;
      // noVNC Websock expects ArrayBuffer in e.data. Slice produces a detached
      // buffer so we avoid aliasing the wasm-owned view.
      const ab = bytes.slice().buffer;
      if (typeof this.onmessage === 'function') {
        this.onmessage({ data: ab });
      }
      return;
    }
    if (variant === 'VncTunnelStreamEnd') {
      this._handleStreamEnd(body.reason || 'stream-end');
    }
  }

  _handleStreamEnd(reason) {
    if (this._closed) return;
    this._closed = true;
    this._readyState = WebSocket.CLOSED;
    this._unsubscribe = null; // server finished — nothing to unsubscribe.
    if (typeof this.onclose === 'function') {
      this.onclose({ code: 1000, reason });
    }
  }

  _handleTransportError(err, onStatus) {
    if (this._closed) return;
    if (!this._opened) {
      onStatus?.({ status: VNC_STATUS_FAILED, error: err?.message || 'transport error' });
    }
    this._readyState = WebSocket.CLOSED;
    if (typeof this.onerror === 'function') this.onerror(err);
    this._teardown('transport-error');
    if (typeof this.onclose === 'function') {
      this.onclose({ code: 1006, reason: 'transport error' });
    }
  }

  /**
   * noVNC sends RFB frames as ArrayBuffer | Uint8Array. We forward them one
   * frame at a time through a one-shot request. Fire-and-forget on happy path,
   * but we surface backend-side rejections through `onerror` so the RFB layer
   * can tear the session down.
   */
  send(data) {
    if (!this._opened || this._closed || !this._tunnelId) return;
    let bytes;
    if (data instanceof Uint8Array) {
      bytes = data;
    } else if (data instanceof ArrayBuffer) {
      bytes = new Uint8Array(data);
    } else if (ArrayBuffer.isView(data)) {
      bytes = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    } else {
      bytes = new Uint8Array(data);
    }
    // Detach from the source buffer so wasm_bindgen Vec<u8> conversion gets a
    // stable view (Websock reuses its send buffer across flushes).
    const owned = bytes.slice();
    const tunnelId = this._tunnelId;
    ApiBinary.one('vncTunnelSendRequest', { tunnelId, bytes: owned })
      .then((resp) => {
        if (resp && resp.ok === false) {
          const err = new Error(resp.error || 'vnc send failed');
          if (typeof this.onerror === 'function') this.onerror(err);
        }
      })
      .catch((err) => {
        if (typeof this.onerror === 'function') this.onerror(err);
      });
  }

  close() {
    if (this._closed) return;
    this._closed = true;
    this._readyState = WebSocket.CLOSED;
    const tunnelId = this._tunnelId;
    this._teardown('client-close');
    if (tunnelId) {
      // Best-effort notify backend. Errors here are not actionable — the WSS
      // close path on the server side also cleans tunnels up.
      ApiBinary.one('vncTunnelCloseRequest', { tunnelId }).catch(() => {});
    }
  }

  _teardown(_reason) {
    const unsub = this._unsubscribe;
    this._unsubscribe = null;
    if (typeof unsub === 'function') {
      try { unsub(); } catch (_) {}
    }
  }
}
