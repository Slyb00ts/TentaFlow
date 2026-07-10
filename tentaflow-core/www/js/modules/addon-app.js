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

// SlotSemantics values whose content is rendered into a dynamic overlay
// container (Modal/Drawer/Sheet/Popover) created by the overlay renderer
// *inside* the host slot, not into a static slot container. The SlotDecl spec
// (tentaflow-sdk-spec) defines `modal`, `drawer`, `popover`; `sheet` is kept
// here for forward compatibility with an SDK-level sheet variant.
const OVERLAY_SEMANTICS = new Set(['modal', 'drawer', 'sheet', 'popover']);

// A slot is overlay-owned when its semantics is an overlay kind OR its
// visibility is Hidden. The intended pattern (host ownership test
// addon/host_functions/ui.rs) is: an overlay slot is declared in PanelShell
// with overlay semantics + Hidden visibility so it passes Rust slot-ownership,
// but its real DOM container is produced dynamically by the overlay renderer.
// Such slots MUST NOT get a static container nor a static registerSlot — the
// dynamic container is auto-registered by SlotManager.observe() and SlotContent
// is buffered until then.
//
// `slotDecl.semantics` decodes to a string enum (e.g. 'modal').
// `slotDecl.visibility` decodes to an object `{ kind: 'always'|'hidden'|... }`
// per SlotVisibility; a bare string is tolerated defensively.
function isOverlaySlot(slotDecl) {
  if (!slotDecl || typeof slotDecl !== 'object') return false;
  if (typeof slotDecl.semantics === 'string'
      && OVERLAY_SEMANTICS.has(slotDecl.semantics)) {
    return true;
  }
  const vis = slotDecl.visibility;
  if (typeof vis === 'string') return vis === 'hidden';
  if (vis && typeof vis === 'object') return vis.kind === 'hidden';
  return false;
}

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
      console.log('[addon-app] unsolicited:', msgBody.variant, msgBody);
      if (msgBody.variant !== 'UiChannelCbor') return;
      handleUiMessage(msgBody.cbor);
    });

    // Send PanelOpen as request-response (pending, not subscribe).
    // The response carries the assigned epoch in a PanelOpen echo.
    const openPromise = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        client.pending.delete(correlationId.toString());
        reject(new Error('PanelOpen timed out'));
      }, 10000);
      client.pending.set(correlationId.toString(), {
        resolve: (result) => { clearTimeout(timer); resolve(result); },
        reject: (err) => { clearTimeout(timer); reject(err); },
      });
    });
    console.log('[addon-app] sending PanelOpen frame, correlationId:', correlationId);
    client._send(frame);

    try {
      console.log('[addon-app] awaiting PanelOpen response...');
      const { envelope: respEnv, body: respBody } = await openPromise;
      console.log('[addon-app] PanelOpen response:', respBody.variant, respBody);
      if (respEnv.isError || respBody.variant === 'Error') {
        showError(respBody.message ?? 'Panel open failed');
        return;
      }
      if (respBody.variant === 'UiChannelCbor' && _session) {
        const decoded = wasm.decodeUiPayload(respBody.cbor);
        if (decoded.assignedEpoch) {
          _session.panelEpoch = Number(decoded.assignedEpoch);
        }
      }
    } catch (e) {
      showError(e.message ?? 'Panel open failed');
      return;
    }
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

  console.log('[addon-app] handleUiMessage, bytes:', cborBytes?.length);

  let decoded;
  try {
    decoded = s.wasm.decodeUiPayload(cborBytes);
  } catch (e) {
    console.error('[addon-app] decodeUiPayload failed:', e);
    return;
  }
  console.log('[addon-app] decoded tag:', '0x' + decoded.tag.toString(16), decoded);

  // Filter messages for this panel
  if (decoded.addonId && decoded.addonId !== s.addonId) return;
  if (decoded.panelId && decoded.panelId !== s.panelId) return;

  switch (decoded.tag) {
    case TAG_PANEL_SHELL:
      try { handlePanelShell(decoded); } catch (e) { console.error('[addon-app] handlePanelShell THREW:', e); }
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
      try { handleSlotContent(decoded); } catch (e) { console.error('[addon-app] handleSlotContent THREW:', e); }
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

  if (decoded.initialState) {
    try {
      s.store.applySnapshot({
        entries: decoded.initialState,
        state_revision: 0,
        truncated: false,
        panel_epoch: decoded.panelEpoch,
      });
    } catch (e) {
      console.error('[addon-app] initial state apply failed:', e);
    }
  }

  // Create event dispatcher that sends actions to backend
  const eventDispatcher = {
    emit({ addon_id, panel_id, panel_epoch, source_id, event_kind, handler, dom_event }) {
      console.log('[event-dispatch]', event_kind, 'handler:', stringifyWithBigInt(handler), 'detail:', dom_event?.detail);
      if (!handler) return;
      if (handler.kind === 'backend' || handler.kind === 'both') {
        const params = { ...(handler.params || {}) };
        if (dom_event && dom_event.detail && typeof dom_event.detail === 'object') {
          Object.assign(params, dom_event.detail);
        }
        sendAction(addon_id, panel_id, panel_epoch, handler.action_id, params);
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

  if (decoded.layout) {
    try {
      const layoutEl = s.renderer.render(decoded.layout);
      shell.appendChild(layoutEl);
    } catch (e) {
      console.error('[addon-app] layout render failed:', e);
      shell.innerHTML = `<p class="error">Layout render error: ${escapeHtml(String(e.message))}</p>`;
    }
  }

  // Tear down a previous SlotManager (and its MutationObserver) before
  // rebuilding the shell — handlePanelShell can run again on panel-navigate.
  if (s.slotManager) s.slotManager.destroy();

  // Create SlotManager and register declared slots
  s.slotManager = new SlotManager({
    store: s.store,
    componentRenderer: s.renderer,
  });

  if (decoded.slots && decoded.slots.length > 0) {
    for (const slotDecl of decoded.slots) {
      // Overlay slots (modal/drawer/sheet/popover or Hidden) are filled by an
      // overlay renderer that creates its own dynamic data-slot-id container
      // inside the host slot. Creating a static container here would shadow
      // that dynamic one (same id) and SlotManager would ignore the real one,
      // dropping the overlay body/footer outside the dialog. Skip both the
      // static container and registerSlot; observe() picks up the dynamic
      // container and handleSlotContent buffers until it is registered.
      if (isOverlaySlot(slotDecl)) continue;

      let slotEl = shell.querySelector(`[data-slot-id="${slotDecl.id}"]`);
      // If layout didn't render a slot placeholder, create one and append
      // to the shell. This is the normal case — the layout contains nav
      // chrome, and slots hold the panel content below it.
      if (!slotEl) {
        slotEl = document.createElement('div');
        slotEl.setAttribute('data-slot-id', slotDecl.id);
        slotEl.classList.add('addon-slot');
        shell.appendChild(slotEl);
      }
      s.slotManager.registerSlot(slotDecl.id, slotEl, slotDecl);
    }
  }

  // Observe the shell so dynamic data-slot-id containers created by overlay
  // renderers (modal/drawer/sheet/popover body+footer) inside existing slots
  // are auto-registered and can receive later SlotContent messages.
  s.slotManager.observe(shell);
}

function handleSlotContent(decoded) {
  const s = _session;
  if (!s || !s.slotManager) return;
  s.slotManager.handleSlotContent({
    slot_id: decoded.slotId,
    fragment: decoded.fragment,
    state_overlay: decoded.stateOverlay,
  });
}

function handleStateSnapshot(decoded) {
  const s = _session;
  if (!s || !s.store) return;

  try {
    s.store.applySnapshot({
      entries: decoded.entries,
      state_revision: decoded.stateRevision,
      truncated: decoded.truncated,
      panel_epoch: decoded.panelEpoch,
    });
  } catch (e) {
    console.error('[addon-app] state snapshot apply failed:', e);
  }
}

function handleStatePatch(decoded) {
  const s = _session;
  if (!s || !s.store) return;

  try {
    s.store.applyPatch({
      base_revision: decoded.baseRevision,
      new_revision: decoded.newRevision,
      ops: decoded.ops,
      panel_epoch: decoded.panelEpoch,
    });
  } catch (e) {
    console.error('[addon-app] state patch apply failed:', e);
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

// CBOR-decoded handler params may contain BigInt (I64 wire values) which
// JSON.stringify rejects with a TypeError. Serialize them as a Number when
// exactly representable (safe-integer range) and as a decimal string
// otherwise — the addon side parses both forms.
function stringifyWithBigInt(value) {
  return JSON.stringify(value, (_key, v) => {
    if (typeof v !== 'bigint') return v;
    return v >= BigInt(Number.MIN_SAFE_INTEGER) && v <= BigInt(Number.MAX_SAFE_INTEGER)
      ? Number(v)
      : v.toString();
  });
}

async function sendAction(addonId, panelId, panelEpoch, actionId, params) {
  if (!_session) return;
  const s = _session;
  try {
    const client = await ApiBinary.client();
    const correlationId = client.nextCorrelationId();
    const sequence = client.takeSequence();
    const paramsJson = stringifyWithBigInt(params ?? {});
    console.log('[addon-app] sendAction:', actionId, paramsJson, 'epoch:', panelEpoch);
    const body = s.wasm.encodeUiAction(
      addonId, panelId, BigInt(panelEpoch), actionId, paramsJson
    );
    const messageKind = s.wasm.messageKind();
    const frame = s.wasm.encodeEnvelopeDirect(
      BigInt(correlationId), BigInt(sequence),
      messageKind.META_HEARTBEAT, body
    );
    client._send(frame);
    console.log('[addon-app] sendAction sent OK, corrId:', correlationId);
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

// Test seam: inject the module-private session so unit tests can drive the
// wire-message handlers without a live WS connection.
function __setSessionForTest(session) {
  _session = session;
}

export default AddonAppScreen;
export { VIEW_ID, isOverlaySlot, handleSlotContent, stringifyWithBigInt, __setSessionForTest };
