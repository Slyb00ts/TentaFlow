// =============================================================================
// File: sdk-runtime/slot-manager.js
// Description: Connects addon wire messages (SlotContent/SlotClear/SlotShow/
// SlotHide) to DOM containers that renderers create with data-slot-id attrs.
// Tracks containers by slot_id, renders fragment Components into them via
// ComponentRenderer, manages visibility, default states and conditional
// visibility subscriptions. A MutationObserver auto-registers/unregisters
// elements with data-slot-id as they enter/leave the DOM.
// =============================================================================

const SLOT_SEMANTICS = new Set([
  'main_content', 'modal', 'drawer', 'toast', 'side_panel',
  'tab_pane', 'popover', 'custom',
]);

const DEFAULT_STATE_KINDS = new Set(['empty', 'loading', 'static']);
const CACHE_POLICY_KINDS = new Set(['none', 'on_navigate_back', 'ttl_seconds']);
const VISIBILITY_KINDS = new Set(['always', 'hidden', 'conditional']);

// linkedom does not expose Node.ELEMENT_NODE as a static constant
const ELEMENT_NODE = globalThis.Node.ELEMENT_NODE ?? 1;

// =============================================================================
// SlotDecl parsing
// =============================================================================

function parseSlotDecl(raw) {
  if (raw == null) return null;
  if (typeof raw !== 'object') {
    throw new TypeError('SlotDecl must be object');
  }
  const decl = {};

  if (raw.semantics != null) {
    if (!SLOT_SEMANTICS.has(raw.semantics)) {
      throw new TypeError(`SlotDecl.semantics: unknown value '${raw.semantics}'`);
    }
    decl.semantics = raw.semantics;
  }

  if (raw.default_state != null) {
    const ds = raw.default_state;
    if (!ds || typeof ds !== 'object' || !DEFAULT_STATE_KINDS.has(ds.kind)) {
      throw new TypeError(`SlotDecl.default_state.kind must be one of: ${[...DEFAULT_STATE_KINDS].join(', ')}`);
    }
    decl.default_state = ds;
  }

  if (raw.cache_policy != null) {
    const cp = raw.cache_policy;
    if (!cp || typeof cp !== 'object' || !CACHE_POLICY_KINDS.has(cp.kind)) {
      throw new TypeError(`SlotDecl.cache_policy.kind must be one of: ${[...CACHE_POLICY_KINDS].join(', ')}`);
    }
    decl.cache_policy = cp;
  }

  if (raw.visibility != null) {
    const vis = raw.visibility;
    if (!vis || typeof vis !== 'object' || !VISIBILITY_KINDS.has(vis.kind)) {
      throw new TypeError(`SlotDecl.visibility.kind must be one of: ${[...VISIBILITY_KINDS].join(', ')}`);
    }
    decl.visibility = vis;
  }

  return decl;
}

// =============================================================================
// SlotManager
// =============================================================================

export class SlotManager {
  /// @param {object} opts
  /// @param {StateStore} opts.store — reactive state store for conditional visibility
  /// @param {ComponentRenderer} opts.componentRenderer — renders fragment Components to DOM
  constructor({ store, componentRenderer } = {}) {
    if (!store || typeof store.subscribe !== 'function') {
      throw new TypeError('SlotManager: store must be a StateStore');
    }
    if (!componentRenderer || typeof componentRenderer.render !== 'function') {
      throw new TypeError('SlotManager: componentRenderer must have .render()');
    }
    this._store = store;
    this._renderer = componentRenderer;
    // Map<string, { element, decl, currentFragment, cleanups: Array<fn> }>
    this._slots = new Map();
    this._observer = null;
    this._destroyed = false;
  }

  // ---------------------------------------------------------------------------
  // Registration
  // ---------------------------------------------------------------------------

  registerSlot(slotId, element, slotDeclRaw) {
    this._assertAlive();
    if (typeof slotId !== 'string' || slotId.length === 0) {
      throw new TypeError('SlotManager.registerSlot: slotId must be non-empty string');
    }
    if (!(element instanceof globalThis.Element)) {
      throw new TypeError('SlotManager.registerSlot: element must be an Element');
    }
    const decl = parseSlotDecl(slotDeclRaw);
    const entry = {
      element,
      decl,
      currentFragment: null,
      cleanups: [],
    };
    this._slots.set(slotId, entry);
    this._applyDefaultState(entry);
    this._applyVisibility(entry);
  }

  unregisterSlot(slotId) {
    const entry = this._slots.get(slotId);
    if (!entry) return;
    this._runCleanups(entry);
    this._slots.delete(slotId);
  }

  // ---------------------------------------------------------------------------
  // Wire message handlers
  // ---------------------------------------------------------------------------

  handleSlotContent({ slot_id, fragment, state_overlay }) {
    this._assertAlive();
    const entry = this._slots.get(slot_id);
    if (!entry) {
      console.warn(`[slot-manager] handleSlotContent: unknown slot '${slot_id}'`);
      return;
    }

    // Apply state overlay before rendering so bindings see updated values
    if (state_overlay != null && Array.isArray(state_overlay) && state_overlay.length > 0) {
      this._store.applyOverlay(state_overlay);
    }

    this._clearContainerContent(entry);

    if (fragment != null) {
      const el = this._renderer.render(fragment);
      entry.element.appendChild(el);
      entry.currentFragment = fragment;
    }
  }

  handleSlotClear({ slot_id }) {
    this._assertAlive();
    const entry = this._slots.get(slot_id);
    if (!entry) {
      console.warn(`[slot-manager] handleSlotClear: unknown slot '${slot_id}'`);
      return;
    }
    this._clearContainerContent(entry);
    entry.currentFragment = null;
    this._applyDefaultState(entry);
  }

  handleSlotShow({ slot_id }) {
    this._assertAlive();
    const entry = this._slots.get(slot_id);
    if (!entry) {
      console.warn(`[slot-manager] handleSlotShow: unknown slot '${slot_id}'`);
      return;
    }
    entry.element.removeAttribute('hidden');
  }

  handleSlotHide({ slot_id }) {
    this._assertAlive();
    const entry = this._slots.get(slot_id);
    if (!entry) {
      console.warn(`[slot-manager] handleSlotHide: unknown slot '${slot_id}'`);
      return;
    }
    entry.element.setAttribute('hidden', '');
  }

  // ---------------------------------------------------------------------------
  // Query
  // ---------------------------------------------------------------------------

  getSlotElement(slotId) {
    const entry = this._slots.get(slotId);
    return entry ? entry.element : null;
  }

  hasSlot(slotId) {
    return this._slots.has(slotId);
  }

  // ---------------------------------------------------------------------------
  // MutationObserver for auto-registration
  // ---------------------------------------------------------------------------

  observe(root) {
    this._assertAlive();
    if (this._observer) return;
    if (!(root instanceof globalThis.Node)) {
      throw new TypeError('SlotManager.observe: root must be a Node');
    }

    // Register any existing data-slot-id elements already in the tree
    const existing = root.querySelectorAll('[data-slot-id]');
    for (const el of existing) {
      const id = el.getAttribute('data-slot-id');
      if (id && !this._slots.has(id)) {
        this.registerSlot(id, el);
      }
    }

    this._observer = new MutationObserver((mutations) => {
      for (const mutation of mutations) {
        for (const node of mutation.addedNodes) {
          if (node.nodeType !== ELEMENT_NODE) continue;
          this._autoRegisterTree(node);
        }
        for (const node of mutation.removedNodes) {
          if (node.nodeType !== ELEMENT_NODE) continue;
          this._autoUnregisterTree(node);
        }
      }
    });

    this._observer.observe(root, { childList: true, subtree: true });
  }

  // ---------------------------------------------------------------------------
  // Lifecycle
  // ---------------------------------------------------------------------------

  destroy() {
    if (this._destroyed) return;
    this._destroyed = true;
    if (this._observer) {
      this._observer.disconnect();
      this._observer = null;
    }
    for (const entry of this._slots.values()) {
      this._runCleanups(entry);
    }
    this._slots.clear();
  }

  // ---------------------------------------------------------------------------
  // Internals
  // ---------------------------------------------------------------------------

  _assertAlive() {
    if (this._destroyed) {
      throw new Error('SlotManager: instance was destroyed');
    }
  }

  _clearContainerContent(entry) {
    // Destroy rendered children via componentRenderer to free store subscriptions
    while (entry.element.firstChild) {
      const child = entry.element.firstChild;
      if (child instanceof globalThis.Element) {
        this._renderer.destroy(child);
      }
      entry.element.removeChild(child);
    }
  }

  _applyDefaultState(entry) {
    if (!entry.decl || !entry.decl.default_state) return;
    const ds = entry.decl.default_state;

    if (ds.kind === 'loading') {
      const spinner = document.createElement('div');
      spinner.classList.add('tf-slot-loading');
      spinner.setAttribute('role', 'status');
      spinner.setAttribute('aria-label', 'Loading');
      entry.element.appendChild(spinner);
    } else if (ds.kind === 'static' && ds.fragment != null) {
      const el = this._renderer.render(ds.fragment);
      entry.element.appendChild(el);
    }
    // 'empty' — leave container empty (no action needed)
  }

  _applyVisibility(entry) {
    if (!entry.decl || !entry.decl.visibility) return;
    const vis = entry.decl.visibility;

    if (vis.kind === 'hidden') {
      entry.element.setAttribute('hidden', '');
    } else if (vis.kind === 'conditional') {
      if (!Array.isArray(vis.path) || vis.path.length === 0) {
        throw new TypeError('SlotDecl.visibility.conditional requires non-empty path');
      }
      // Initial check
      const applyVis = () => {
        const v = this._store.read(vis.path);
        if (v) {
          entry.element.removeAttribute('hidden');
        } else {
          entry.element.setAttribute('hidden', '');
        }
      };
      applyVis();
      const unsub = this._store.subscribe(vis.path, applyVis);
      entry.cleanups.push(unsub);
    }
    // 'always' — no action needed (default visible)
  }

  _runCleanups(entry) {
    for (const fn of entry.cleanups) {
      try {
        fn();
      } catch (e) {
        console.error('[slot-manager] cleanup threw:', e);
      }
    }
    entry.cleanups.length = 0;
  }

  _autoRegisterTree(node) {
    const id = node.getAttribute && node.getAttribute('data-slot-id');
    if (id && !this._slots.has(id)) {
      this.registerSlot(id, node);
    }
    if (node.querySelectorAll) {
      const descendants = node.querySelectorAll('[data-slot-id]');
      for (const el of descendants) {
        const elId = el.getAttribute('data-slot-id');
        if (elId && !this._slots.has(elId)) {
          this.registerSlot(elId, el);
        }
      }
    }
  }

  _autoUnregisterTree(node) {
    const id = node.getAttribute && node.getAttribute('data-slot-id');
    if (id && this._slots.has(id)) {
      this.unregisterSlot(id);
    }
    if (node.querySelectorAll) {
      const descendants = node.querySelectorAll('[data-slot-id]');
      for (const el of descendants) {
        const elId = el.getAttribute('data-slot-id');
        if (elId && this._slots.has(elId)) {
          this.unregisterSlot(elId);
        }
      }
    }
  }
}
