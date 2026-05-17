// =============================================================================
// File: addon-ui-host.js
// Description: Parent-side harness for addon UI iframes. Owns the global
//              `window message` listener, the iframe→{addonId, permissions}
//              registry, postMessage dispatch with strict validation, and the
//              action→host-function routing (binary WS for wired actions,
//              EUNIMPL for not-yet-backed actions, local handlers for ui.*).
// Example:
//   import { addonUiHost } from '/js/addon-ui-host.js';
//   const handle = addonUiHost.mount({
//     addonId: 'tentavision',
//     componentId: 'dashboard',
//     bundleHtml: '<!DOCTYPE html>...',
//     permissions: ['alias.read'],
//     containerEl: document.getElementById('mount'),
//   });
//   handle.unmount();
// =============================================================================

import {
  ACTION_PERMISSION_MAP,
  ACTION_REGISTRY,
  bridgeError,
  validateRequestEnvelope,
} from './addon-ui-host/bridge-schema.js';

// Lazy import so the demo page can load the host without booting the binary
// transport — initBinaryClient() is called only when an addon issues a
// backend-bound action.
let _apiBinaryPromise = null;
async function getApiBinary() {
  if (!_apiBinaryPromise) {
    _apiBinaryPromise = import('./protocol/api-binary-shim.js').then((m) => m.ApiBinary);
  }
  return _apiBinaryPromise;
}

// Maps each live iframe element to its registration record. We key on the
// element itself (NOT addonId) because event.source equality identifies the
// iframe; the addonId in any user-controlled payload would be impersonable.
const _iframeRegistry = new Map();

// ---------- global listener (installed once) ---------------------------------

let _listenerInstalled = false;

function ensureGlobalListener() {
  if (_listenerInstalled) return;
  _listenerInstalled = true;
  window.addEventListener('message', onWindowMessage);
}

function onWindowMessage(event) {
  // Find the registration whose iframe contentWindow matches event.source.
  // Linear scan — registry is expected to hold at most a handful of frames.
  let record = null;
  let iframe = null;
  for (const [el, rec] of _iframeRegistry) {
    if (el.contentWindow === event.source) {
      record = rec;
      iframe = el;
      break;
    }
  }
  if (!record) return; // not one of ours

  handleMessage(iframe, record, event.data).catch((err) => {
    console.error('[addon-ui-host] handler threw:', err);
  });
}

async function handleMessage(iframe, record, raw) {
  let entry;
  try {
    entry = validateRequestEnvelope(raw);
  } catch (err) {
    // We need an id to respond meaningfully. If the envelope was so malformed
    // we cannot even pull an id, drop the message silently — the iframe gets
    // no response and its own request promise will time out client-side.
    const maybeId = raw && typeof raw === 'object' ? raw.id : null;
    if (typeof maybeId === 'string') {
      respond(iframe, maybeId, false, null, {
        code: err.bridgeCode || 'EBADREQ',
        message: err.message,
      });
    }
    return;
  }

  // Permission check (auto-derived from manifest host_permissions).
  const requiredScope = ACTION_PERMISSION_MAP[raw.action];
  if (requiredScope !== null && !record.permissions.has(requiredScope)) {
    respond(iframe, raw.id, false, null, {
      code: 'EPERM',
      message: `addon lacks permission "${requiredScope}" for action "${raw.action}"`,
    });
    return;
  }

  try {
    const result = await dispatchAction(record, raw.action, raw.payload, entry);
    respond(iframe, raw.id, true, result, null);
  } catch (err) {
    respond(iframe, raw.id, false, null, {
      code: err.bridgeCode || 'EINTERNAL',
      message: err.message || 'internal error',
    });
  }
}

function respond(iframe, id, ok, result, error) {
  const msg = ok
    ? { kind: 'response', id, ok: true, result }
    : { kind: 'response', id, ok: false, error };
  // Target origin '*' is acceptable here because the iframe has a unique
  // opaque origin (sandbox without allow-same-origin) — no other origin can
  // observe the postMessage. Restricting would require pinning to "null".
  iframe.contentWindow?.postMessage(msg, '*');
}

// ---------- action dispatchers -----------------------------------------------

async function dispatchAction(record, action, payload, entry) {
  if (entry.backend === 'local') {
    return dispatchLocal(action, payload);
  }
  if (entry.backend === 'binary') {
    return dispatchBinary(record, action, payload);
  }
  // entry.backend === 'unimpl'
  throw bridgeError('EUNIMPL', `action "${action}" is not yet wired to a backend host function`);
}

async function dispatchBinary(record, action, payload) {
  const api = await getApiBinary();
  switch (action) {
    case 'alias.list_owned': {
      const all = await api.list('modelAliasListRequest', { arrayKey: 'aliases' });
      // Filter to aliases owned by this addon. The binary response includes
      // owner_addon_id per row (see codec.modelAliasListRequest schema).
      return all.filter((a) => a.owner_addon_id === record.addonId);
    }
    default:
      throw bridgeError('EINTERNAL', `binary dispatch missing for "${action}"`);
  }
}

function dispatchLocal(action, payload) {
  switch (action) {
    case 'ui.notify':
      emitToast(payload.level, payload.message);
      return {};
    default:
      throw bridgeError('EINTERNAL', `local dispatch missing for "${action}"`);
  }
}

function emitToast(level, message) {
  // Surface to whatever toast layer the host page provides; falls back to a
  // CustomEvent so test fixtures can observe without pulling a UI dep.
  window.dispatchEvent(new CustomEvent('tf-addon-toast', {
    detail: { level, message },
  }));
}

// ---------- public API --------------------------------------------------------

export const addonUiHost = {
  /**
   * Mount an addon UI bundle into the given container. Returns a handle with
   * `unmount()` and the underlying `<iframe>` for tests.
   *
   * Required opts:
   *   addonId, componentId, permissions (string[]), containerEl,
   *   and one of: { bundleHtml: string } | { srcUrl: string }
   */
  mount(opts) {
    if (!opts || typeof opts !== 'object') {
      throw new Error('addonUiHost.mount: opts required');
    }
    const { addonId, componentId, permissions, containerEl, bundleHtml, srcUrl } = opts;
    if (typeof addonId !== 'string' || !addonId) {
      throw new Error('addonUiHost.mount: addonId required');
    }
    if (typeof componentId !== 'string' || !componentId) {
      throw new Error('addonUiHost.mount: componentId required');
    }
    if (!Array.isArray(permissions)) {
      throw new Error('addonUiHost.mount: permissions must be an array');
    }
    if (!(containerEl instanceof HTMLElement)) {
      throw new Error('addonUiHost.mount: containerEl must be an HTMLElement');
    }
    if (!bundleHtml && !srcUrl) {
      throw new Error('addonUiHost.mount: bundleHtml or srcUrl required');
    }

    ensureGlobalListener();

    const frame = document.createElement('tf-addon-ui-frame');
    frame.setAttribute('addon-id', addonId);
    frame.setAttribute('component-id', componentId);
    frame.setAttribute('permissions', permissions.join(','));

    let ownedBlobUrl = null;
    if (bundleHtml) {
      const blob = new Blob([bundleHtml], { type: 'text/html' });
      ownedBlobUrl = URL.createObjectURL(blob);
      frame.setAttribute('src-url', ownedBlobUrl);
    } else {
      frame.setAttribute('src-url', srcUrl);
    }

    containerEl.appendChild(frame);

    // The component constructs its inner <iframe> on connectedCallback. Wait
    // for the next microtask so registry registration sees the real element.
    const register = () => {
      const innerIframe = frame.iframe;
      if (!innerIframe) {
        queueMicrotask(register);
        return;
      }
      _iframeRegistry.set(innerIframe, {
        addonId,
        componentId,
        permissions: new Set(permissions),
      });
    };
    register();

    return {
      element: frame,
      get iframe() { return frame.iframe; },
      unmount() {
        const innerIframe = frame.iframe;
        if (innerIframe) _iframeRegistry.delete(innerIframe);
        if (ownedBlobUrl) {
          URL.revokeObjectURL(ownedBlobUrl);
          ownedBlobUrl = null;
        }
        frame.remove();
      },
    };
  },

  // Test-only introspection. Not part of the production surface.
  _registrySize() { return _iframeRegistry.size; },
};
