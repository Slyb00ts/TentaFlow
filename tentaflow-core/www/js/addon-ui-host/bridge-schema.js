// =============================================================================
// File: addon-ui-host/bridge-schema.js
// Description: Strict JSON-shape validators + action→permission mapping for the
//              addon-ui postMessage bridge. Vanilla JS (no Ajv) to keep the
//              parent harness dependency-free; each action declares input and
//              output validators that throw a typed { code, message } error on
//              shape mismatch, which the host translates into an EBADREQ
//              response back to the iframe.
// =============================================================================

// Action namespace → backend permission scope (auto-derived per F1c-P0 Q4).
// An addon may invoke `action` only if its manifest host_permissions list
// includes the mapped scope. Adding a new action means adding a row here AND
// declaring it in ACTION_REGISTRY below.
export const ACTION_PERMISSION_MAP = Object.freeze({
  'alias.list_owned': 'alias.read',
  'camera.list':      'camera.read',
  'camera.snapshot':  'camera.snapshot',
  'vector.search':    'vector.read',
  'ui.notify':        null,
});

// ---------- primitive validators ----------------------------------------------

function isPlainObject(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

function expectObject(value, path) {
  if (!isPlainObject(value)) {
    throw bridgeError('EBADREQ', `${path} must be an object`);
  }
}

function expectString(value, path, { allowEmpty = false } = {}) {
  if (typeof value !== 'string') {
    throw bridgeError('EBADREQ', `${path} must be a string`);
  }
  if (!allowEmpty && value.length === 0) {
    throw bridgeError('EBADREQ', `${path} must not be empty`);
  }
}

function expectIntInRange(value, path, min, max) {
  if (!Number.isInteger(value) || value < min || value > max) {
    throw bridgeError('EBADREQ', `${path} must be an integer in [${min},${max}]`);
  }
}

function expectArrayOfNumbers(value, path, { minLen = 1, maxLen = 4096 } = {}) {
  if (!Array.isArray(value)) {
    throw bridgeError('EBADREQ', `${path} must be an array`);
  }
  if (value.length < minLen || value.length > maxLen) {
    throw bridgeError('EBADREQ', `${path} length must be in [${minLen},${maxLen}]`);
  }
  for (let i = 0; i < value.length; i += 1) {
    if (typeof value[i] !== 'number' || !Number.isFinite(value[i])) {
      throw bridgeError('EBADREQ', `${path}[${i}] must be a finite number`);
    }
  }
}

function expectOneOf(value, path, allowed) {
  if (!allowed.includes(value)) {
    throw bridgeError('EBADREQ', `${path} must be one of ${JSON.stringify(allowed)}`);
  }
}

export function bridgeError(code, message) {
  const err = new Error(message);
  err.bridgeCode = code;
  return err;
}

// ---------- action registry ---------------------------------------------------

export const ACTION_REGISTRY = Object.freeze({
  // List aliases owned by the calling addon. Backend wired to
  // modelAliasListRequest filtered by addon_id in the host dispatcher.
  'alias.list_owned': {
    backend: 'binary',
    validateInput(payload) {
      expectObject(payload, 'payload');
    },
  },

  // Camera list — backend host fn not yet exposed via binary protocol.
  // P1 returns EUNIMPL until a camera admin endpoint lands.
  'camera.list': {
    backend: 'unimpl',
    validateInput(payload) {
      expectObject(payload, 'payload');
    },
  },

  'camera.snapshot': {
    backend: 'unimpl',
    validateInput(payload) {
      expectObject(payload, 'payload');
      expectString(payload.camera_id, 'payload.camera_id');
    },
  },

  // Vector search — backend lands in F1c-P3; P1 ships the schema so addon
  // authors can code against the final action contract.
  'vector.search': {
    backend: 'unimpl',
    validateInput(payload) {
      expectObject(payload, 'payload');
      expectString(payload.namespace, 'payload.namespace');
      expectArrayOfNumbers(payload.query, 'payload.query');
      expectIntInRange(payload.k, 'payload.k', 1, 1000);
    },
  },

  // Local-only: parent triggers a toast. No backend round-trip.
  'ui.notify': {
    backend: 'local',
    validateInput(payload) {
      expectObject(payload, 'payload');
      expectOneOf(payload.level, 'payload.level', ['info', 'warn', 'error']);
      expectString(payload.message, 'payload.message');
    },
  },
});

// ---------- envelope validator ------------------------------------------------

export function validateRequestEnvelope(msg) {
  if (!isPlainObject(msg)) throw bridgeError('EBADREQ', 'message must be an object');
  if (msg.kind !== 'request') throw bridgeError('EBADREQ', 'kind must be "request"');
  if (typeof msg.id !== 'string' || msg.id.length === 0 || msg.id.length > 128) {
    throw bridgeError('EBADREQ', 'id must be a non-empty string ≤128 chars');
  }
  if (typeof msg.action !== 'string' || msg.action.length === 0) {
    throw bridgeError('EBADREQ', 'action must be a non-empty string');
  }
  if (!Object.prototype.hasOwnProperty.call(ACTION_REGISTRY, msg.action)) {
    throw bridgeError('EUNKNOWN_ACTION', `unknown action: ${msg.action}`);
  }
  const entry = ACTION_REGISTRY[msg.action];
  entry.validateInput(msg.payload);
  return entry;
}
