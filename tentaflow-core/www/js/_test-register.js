// ===== File: _test-register.js — installs the root-path resolver before any test module loads =====
import { register } from 'node:module';
import { after } from 'node:test';

register('./_test-resolver.js', import.meta.url);

// Unit tests never talk to a node. Node's own WebSocket would dial the happy-dom origin, and once the
// wasm codec initialises (the wasm2js twin does under Node) any unstubbed ApiBinary call, such as the
// i18n sync after a language change, reaches that dial and starts the client's reconnect loop. This
// default socket fails at once and asynchronously, the way a refused connection does; tests that need
// a live-looking socket assign their own `globalThis.WebSocket`.
globalThis.WebSocket = class RefusedWebSocket {
  constructor(url) {
    this.url = String(url);
    this.readyState = 3;
    queueMicrotask(() => this.onerror?.({ type: 'error', target: this }));
  }
  send() { throw new Error('RefusedWebSocket: not connected'); }
  close() {}
  addEventListener() {}
  removeEventListener() {}
};

// An anchored menu left open watches its trigger on animation frames, which would keep a test process
// alive after its last test; closing the leftovers lets the process end the way it does for any other file.
after(() => {
  for (const menu of globalThis.document?.querySelectorAll('tf-menu') ?? []) menu.remove();
});
