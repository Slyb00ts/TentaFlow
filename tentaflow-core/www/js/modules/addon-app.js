// =============================================================================
// File: modules/addon-app.js
// Purpose: SDK-runtime-based addon panel host. Sends PanelOpen/PanelClose via
// binary WS, receives UiChannelCbor pushes (PanelShell, SlotContent, StatePatch,
// etc.) and feeds them into ComponentRenderer + StateStore + SlotManager.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, byId } from '/js/utils.js';
import { codecReady } from '/js/protocol/codec.js';
import { bootstrapSdkRuntime, SlotManager } from '/js/sdk-runtime/bootstrap.js';
import { ComponentRenderer } from '/js/sdk-runtime/component-renderer.js';
import { StateStore } from '/js/sdk-runtime/state-store.js';

const VIEW_ID = 'addon-app';

// Module-level session state; reset on each show().
let _session = null;

// Tags from UiPayload spec
const TAG_PANEL_SHELL    = 0x0102;
const TAG_PANEL_READY    = 0x0103;
const TAG_PANEL_ERROR    = 0x0104;
const TAG_PANEL_RESET    = 0x0106;
const TAG_SLOT_CONTENT   = 0x0110;
const TAG_SLOT_CLEAR     = 0x0111;
const TAG_SLOT_SHOW      = 0x0112;
const TAG_SLOT_HIDE      = 0x0113;
const TAG_STATE_SNAPSHOT = 0x0120;
const TAG_STATE_PATCH    = 0x0121;
const TAG_STATE_RESET    = 0x0122;
const TAG_PATCH_REJECTED = 0x0123;
const TAG_ACTION_ACK     = 0x0131;

const AddonAppScreen = {
  async show(params = {}) {
    teardown();

    const addonId = String(params.addonId ?? params.addon_id ?? '');
    const panelId = String(params.panelId ?? params.panel_id ?? '');
    const main = byId('main');
    if (!main) return;

    if (!addonId || !panelId) {
      main.innerHTML = `<div class="addon-app-shell"><p class="error">Missing addonId / panelId.</p></div>`;
      return;
    }

    main.innerHTML = `
      <div class="addon-app-shell" data-addon="${escapeHtml(addonId)}" data-panel="${escapeHtml(panelId)}">
        <div class="addon-app-loading">Loading panel...</div>
      </div>`;

    const wasm = await codecReady;
    bootstrapSdkRuntime();

    const client = await ApiBinary.client();
    const correlationId = client.nextCorrelationId();
    const sequence = client.takeSequence();

    // Encode PanelOpen body and wrap in envelope
    const body = wasm.encodeUiPanelOpen(
      addonId, panelId, navigator.language || 'en', 'dark',
      window.innerWidth, window.innerHeight
    );
    const messageKind = wasm.messageKind();
    const frame = wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence),
      messageKind.META_HEARTBEAT, body
    );

    // Session tracks all state for the active panel
    _session = {
      addonId,
      panelId,
      panelEpoch: 0,
      wasm,
      store: null,
      renderer: null,
      slotManager: null,
      unsubUnsolicited: null,
    };

    // Subscribe to unsolicited pushes for this panel
    _session.unsubUnsolicited = client.addUnsolicitedListener(({ envelope, body: msgBody }) => {
      if (msgBody.variant !== 'UiChannelCbor') return;
      handleUiMessage(msgBody.cbor);
    });

    // Send PanelOpen and handle response
    client.subscribe(correlationId, ({ envelope, body: respBody }) => {
      if (envelope.isError || respBody.variant === 'Error') {
        showError(respBody.message ?? 'Panel open failed');
        return;
      }
      if (respBody.variant === 'UiChannelCbor') {
        handleUiMessage(respBody.cbor);
      }
    });
    client._send(frame);
  },

  unmount() {
    teardown();
  },
};

function teardown() {
  if (!_session) return;
  const s = _session;
  _session = null;

  if (s.unsubUnsolicited) s.unsubUnsolicited();
  if (s.slotManager) s.slotManager.destroy();

  // Send PanelClose if we have an epoch
  if (s.panelEpoch > 0) {
    sendPanelClose(s.addonId, s.panelId, s.panelEpoch, s.wasm).catch(() => {});
  }

  // Remove overlay portals
  document.querySelectorAll('[data-sdk-overlay-portal]').forEach((n) => {
    if (typeof n.__popoverCleanup === 'function') {
      try { n.__popoverCleanup(); } catch {}
    }
    n.remove();
  });
}

async function sendPanelClose(addonId, panelId, epoch, wasm) {
  const client = await ApiBinary.client();
  const correlationId = client.nextCorrelationId();
  const sequence = client.takeSequence();
  const body = wasm.encodeUiPanelClose(addonId, panelId, BigInt(epoch));
  const messageKind = wasm.messageKind();
  const frame = wasm.encodeEnvelopeDirect(
    BigInt(correlationId), BigInt(sequence),
    messageKind.META_HEARTBEAT, body
  );
  client._send(frame);
}

function handleUiMessage(cborBytes) {
  if (!_session) return;
  const s = _session;

  let decoded;
  try {
    decoded = s.wasm.decodeUiPayload(cborBytes);
  } catch (e) {
    console.error('[addon-app] decodeUiPayload failed:', e);
    return;
  }

  // Filter messages for this panel
  if (decoded.addonId && decoded.addonId !== s.addonId) return;
  if (decoded.panelId && decoded.panelId !== s.panelId) return;

  switch (decoded.tag) {
    case TAG_PANEL_SHELL:
      handlePanelShell(decoded);
      break;
    case TAG_PANEL_READY:
      break;
    case TAG_PANEL_ERROR:
      showError(decoded.message || 'Panel error');
      break;
    case TAG_PANEL_RESET:
      handlePanelReset(decoded);
      break;
    case TAG_SLOT_CONTENT:
      handleSlotContent(decoded);
      break;
    case TAG_SLOT_CLEAR:
      if (s.slotManager) s.slotManager.handleSlotClear({ slot_id: decoded.slotId });
      break;
    case TAG_SLOT_SHOW:
      if (s.slotManager) s.slotManager.handleSlotShow({ slot_id: decoded.slotId });
      break;
    case TAG_SLOT_HIDE:
      if (s.slotManager) s.slotManager.handleSlotHide({ slot_id: decoded.slotId });
      break;
    case TAG_STATE_SNAPSHOT:
      handleStateSnapshot(decoded);
      break;
    case TAG_STATE_PATCH:
      handleStatePatch(decoded);
      break;
    case TAG_STATE_RESET:
      if (s.store) s.store.applyReset({ panel_epoch: decoded.panelEpoch, new_revision: decoded.newRevision });
      break;
    case TAG_PATCH_REJECTED:
      break;
    case TAG_ACTION_ACK:
      break;
    default:
      break;
  }
}

function handlePanelShell(decoded) {
  const s = _session;
  if (!s) return;

  s.panelEpoch = decoded.panelEpoch;

  // Create StateStore
  s.store = new StateStore({
    addon_id: s.addonId,
    panel_id: s.panelId,
    panel_epoch: decoded.panelEpoch,
  });

  // Apply initial state if present
  if (decoded.initialStateCbor && decoded.initialStateCbor.byteLength > 0) {
    try {
      const entries = s.wasm.decodeStateEntriesCbor(decoded.initialStateCbor);
      s.store.applySnapshot({
        entries,
        state_revision: 0,
        truncated: false,
        panel_epoch: decoded.panelEpoch,
      });
    } catch (e) {
      console.error('[addon-app] initial state decode failed:', e);
    }
  }

  // Create event dispatcher that sends actions to backend
  const eventDispatcher = {
    emit({ addon_id, panel_id, panel_epoch, source_id, event_kind, handler, dom_event }) {
      if (!handler) return;
      if (handler.kind === 'backend' || handler.kind === 'both') {
        sendAction(addon_id, panel_id, panel_epoch, handler.action_id, handler.params || {});
      }
      // Local actions handled by renderer infrastructure
    },
  };

  // Create ComponentRenderer
  s.renderer = new ComponentRenderer({
    store: s.store,
    eventDispatcher,
    locale: navigator.language,
  });

  // Render the layout shell
  const shell = byId('main')?.querySelector('.addon-app-shell');
  if (!shell) return;

  shell.innerHTML = '';

  if (decoded.layoutCbor && decoded.layoutCbor.byteLength > 0) {
    try {
      const layoutComponent = s.wasm.decodeComponentCbor(decoded.layoutCbor);
      const layoutEl = s.renderer.render(layoutComponent);
      shell.appendChild(layoutEl);
    } catch (e) {
      console.error('[addon-app] layout render failed:', e);
      shell.innerHTML = `<p class="error">Layout render error: ${escapeHtml(String(e.message))}</p>`;
    }
  }

  // Create SlotManager and register declared slots
  s.slotManager = new SlotManager({
    store: s.store,
    componentRenderer: s.renderer,
  });

  if (decoded.slots && decoded.slots.length > 0) {
    for (const slotDecl of decoded.slots) {
      const slotEl = shell.querySelector(`[data-slot-id="${slotDecl.id}"]`);
      if (slotEl) {
        s.slotManager.registerSlot(slotDecl.id, slotEl, slotDecl);
      }
    }
  }
}

function handleSlotContent(decoded) {
  const s = _session;
  if (!s || !s.slotManager) return;

  let fragment = null;
  if (decoded.fragmentCbor && decoded.fragmentCbor.byteLength > 0) {
    try {
      fragment = s.wasm.decodeComponentCbor(decoded.fragmentCbor);
    } catch (e) {
      console.error('[addon-app] fragment decode failed:', e);
      return;
    }
  }

  s.slotManager.handleSlotContent({ slot_id: decoded.slotId, fragment });
}

function handleStateSnapshot(decoded) {
  const s = _session;
  if (!s || !s.store) return;

  try {
    const entries = s.wasm.decodeStateEntriesCbor(decoded.entriesCbor);
    s.store.applySnapshot({
      entries,
      state_revision: decoded.stateRevision,
      truncated: decoded.truncated,
      panel_epoch: decoded.panelEpoch,
    });
  } catch (e) {
    console.error('[addon-app] state snapshot decode failed:', e);
  }
}

function handleStatePatch(decoded) {
  const s = _session;
  if (!s || !s.store) return;

  try {
    const ops = s.wasm.decodePatchOpsCbor(decoded.opsCbor);
    s.store.applyPatch({
      base_revision: decoded.baseRevision,
      new_revision: decoded.newRevision,
      ops,
      panel_epoch: decoded.panelEpoch,
    });
  } catch (e) {
    console.error('[addon-app] state patch decode failed:', e);
  }
}

function handlePanelReset(decoded) {
  // Full re-open with new epoch
  const s = _session;
  if (!s) return;
  s.panelEpoch = decoded.newPanelEpoch;
  if (s.store) {
    s.store.applyReset({ panel_epoch: decoded.newPanelEpoch, new_revision: 0 });
  }
}

async function sendAction(addonId, panelId, panelEpoch, actionId, params) {
  if (!_session) return;
  const s = _session;
  try {
    const client = await ApiBinary.client();
    const correlationId = client.nextCorrelationId();
    const sequence = client.takeSequence();
    const paramsJson = JSON.stringify(params ?? {});
    const body = s.wasm.encodeUiAction(
      addonId, panelId, BigInt(panelEpoch), actionId, paramsJson
    );
    const messageKind = s.wasm.messageKind();
    const frame = s.wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence),
      messageKind.META_HEARTBEAT, body
    );
    client._send(frame);
  } catch (e) {
    console.error('[addon-app] sendAction failed:', e);
  }
}

function showError(message) {
  const shell = byId('main')?.querySelector('.addon-app-shell');
  if (shell) {
    shell.innerHTML = `<p class="error">${escapeHtml(message)}</p>`;
  }
}

export default AddonAppScreen;
export { VIEW_ID };
