// =============================================================================
// File: sdk-runtime/slot-manager.js
// Description: Connects addon wire messages (SlotContent/SlotClear/SlotShow/
// SlotHide) to DOM containers that renderers create with data-slot-id attrs.
// Tracks containers by slot_id, renders fragment Components into them via
// ComponentRenderer, manages visibility, default states and conditional
// visibility subscriptions. A MutationObserver auto-registers/unregisters
// elements with data-slot-id as they enter/leave the DOM.
// =============================================================================

import {
  transparentContainerChildKey,
  readContainerChildren,
  shellEqualsExceptChildKey,
  componentDeepEqual,
  validateFragmentTree,
} from './component-renderer.js';

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
    // Buffer for SlotContent that arrived before its container was registered.
    // Overlay renderers (modal/drawer body+footer) create dynamic data-slot-id
    // containers that the MutationObserver auto-registers asynchronously, so a
    // SlotContent sent right after the panel content can race ahead of the
    // registration. We keep the last content per slot_id and replay it on
    // register. Map<string, { fragment, state_overlay }> (last write wins).
    this._pendingContent = new Map();
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
      slotId,
      element,
      decl,
      currentFragment: null,
      cleanups: [],
    };
    this._slots.set(slotId, entry);
    this._applyDefaultState(entry);
    this._applyVisibility(entry);

    // Replay any content buffered before this slot existed (dynamic overlay
    // containers auto-registered after their SlotContent already arrived).
    const pending = this._pendingContent.get(slotId);
    if (pending) {
      this._pendingContent.delete(slotId);
      this._applySlotContent(entry, pending.fragment, pending.state_overlay);
    }
  }

  unregisterSlot(slotId) {
    // A removed overlay container must not leave a stale pending replay behind.
    this._pendingContent.delete(slotId);
    const entry = this._slots.get(slotId);
    if (!entry) return;
    // Tear down rendered content too, otherwise the fragment's live store
    // subscriptions outlive the slot and leak.
    this._clearContainerContent(entry);
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
      // Container not registered yet — buffer the latest content and replay it
      // when the slot is registered (dynamic overlay containers register
      // asynchronously via MutationObserver). Last write wins.
      this._pendingContent.set(slot_id, { fragment, state_overlay });
      return;
    }

    this._applySlotContent(entry, fragment, state_overlay);
  }

  _applySlotContent(entry, fragment, state_overlay) {
    // Apply state overlay before rendering so bindings (and reconcile, which
    // leaves unchanged reactive nodes alone) see the updated store values.
    if (state_overlay != null && Array.isArray(state_overlay) && state_overlay.length > 0) {
      this._store.applyOverlay(state_overlay);
    }

    if (fragment == null) {
      // Explicit empty fragment — tear the slot down (mirrors clear).
      this._clearContainerContent(entry);
      entry.currentFragment = null;
      return;
    }

    // Validate the new fragment with the SAME rigor as ComponentRenderer.render
    // BEFORE any DOM patching. The reconcile path reuses the wrapper and its
    // helpers (fieldsToMap/readContainerChildren) silently tolerate malformed or
    // duplicate-key FieldMap entries, so without this a bad fragment could be
    // accepted (stale DOM + a wrong currentFragment). On throw we leave DOM and
    // currentFragment untouched; handleSlotContent's caller logs it.
    validateFragmentTree(fragment, 'SlotManager._applySlotContent');

    const existingRoot = entry.element.firstChild;
    const prev = entry.currentFragment;

    // Reconcile in place only when we already have a rendered tree, the slot
    // currently holds exactly that one root element, and the root tag matches.
    // Otherwise fall back to clear+render (first render, structural mismatch,
    // or a slot that default-state put extra nodes into).
    const canReconcile =
      prev != null &&
      existingRoot instanceof globalThis.Element &&
      entry.element.childNodes.length === 1 &&
      prev.tag === fragment.tag;

    if (canReconcile) {
      this._reconcile(existingRoot, prev, fragment);
      entry.currentFragment = fragment;
      return;
    }

    this._clearContainerContent(entry);
    const el = this._renderer.render(fragment);
    entry.element.appendChild(el);
    entry.currentFragment = fragment;
  }

  /// Patch `domNode` (currently representing `oldComp`) toward `newComp`,
  /// reusing DOM nodes wherever it is provably safe so focus, scroll and
  /// in-progress input values survive an addon re-pushing the same SlotContent.
  ///
  /// Strategy (intentionally conservative — see component-renderer for why a
  /// generic per-element attribute re-apply is not possible with tag handlers
  /// that own their internal DOM and closures):
  ///   1. tag changed             → render new + replace + destroy old.
  ///   2. transparent container,
  ///      shell unchanged          → REUSE the wrapper element, recurse into
  ///                                 children 1:1 (this is what keeps a focused
  ///                                 input alive across re-pushes).
  ///   3. anything else            → if the component is byte-identical, leave
  ///                                 the node and its live subscriptions
  ///                                 untouched; otherwise render new + replace.
  _reconcile(domNode, oldComp, newComp) {
    if (oldComp.tag !== newComp.tag) {
      this._replaceNode(domNode, newComp);
      return;
    }

    const childKey = transparentContainerChildKey(newComp.tag);
    const isContainer = childKey !== undefined;

    // For a transparent container we can only reuse the wrapper when its own
    // shell (classes/padding/handlers/bind/a11y) is unchanged — those are set
    // by the tag handler from non-child fields and cannot be re-applied to an
    // existing element without re-running the renderer.
    if (isContainer && shellEqualsExceptChildKey(oldComp, newComp, childKey)) {
      this._reconcileChildren(domNode, oldComp, newComp, childKey);
      return;
    }

    // Leaf node (or container whose shell changed): if nothing changed at all,
    // keep the node and its live store subscriptions exactly as-is. This is the
    // common case for inputs that merely re-arrive in an unchanged fragment —
    // zero DOM churn, focus and value preserved by definition.
    if (componentDeepEqual(oldComp, newComp)) return;

    // Something in this subtree changed and we cannot patch it in place safely
    // → render fresh and swap just this node.
    this._replaceNode(domNode, newComp);
  }

  _reconcileChildren(parentEl, oldComp, newComp, childKey) {
    const oldChildren = readContainerChildren(oldComp);
    const newChildren = readContainerChildren(newComp);
    // If either side is not a clean Array<Component> the 1:1 assumption breaks
    // — fall back to replacing the whole container subtree.
    if (oldChildren == null || newChildren == null) {
      this._replaceNode(parentEl, newComp);
      return;
    }

    const domChildren = parentEl.childNodes;
    const common = Math.min(oldChildren.length, newChildren.length);

    // Common prefix — recurse pairwise on the DOM child elements.
    for (let i = 0; i < common; i++) {
      const childDom = domChildren[i];
      if (!(childDom instanceof globalThis.Element)) {
        // DOM/child-array desync (should not happen for transparent
        // containers) — bail out to a safe full replace of the container.
        this._replaceNode(parentEl, newComp);
        return;
      }
      this._reconcile(childDom, oldChildren[i], newChildren[i]);
    }

    // Surplus old children removed from the tail.
    for (let i = oldChildren.length - 1; i >= newChildren.length; i--) {
      const childDom = domChildren[i];
      if (childDom instanceof globalThis.Element) {
        this._renderer.destroy(childDom);
      }
      if (childDom) parentEl.removeChild(childDom);
    }

    // Surplus new children appended to the tail.
    for (let i = oldChildren.length; i < newChildren.length; i++) {
      const childEl = this._renderer.render(newChildren[i]);
      parentEl.appendChild(childEl);
    }
  }

  /// Render `newComp` fresh and swap it in for `domNode`, freeing the old
  /// subtree's store subscriptions / DOM listeners. Used whenever in-place
  /// patching is unsafe (tag change, changed container shell, changed leaf).
  _replaceNode(domNode, newComp) {
    const fresh = this._renderer.render(newComp);
    domNode.replaceWith(fresh);
    if (domNode instanceof globalThis.Element) {
      this._renderer.destroy(domNode);
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
      // Process ALL removals across the whole batch BEFORE ANY additions.
      // A reconcile _replaceNode (element.replaceWith(newEl)) on a component that
      // carries a dynamic data-slot-id (e.g. modal body/footer) yields a single
      // mutation record with removedNodes=[old] AND addedNodes=[new] for the same
      // slot_id. If we registered the new node first, the guard `!_slots.has(id)`
      // would skip it because the stale entry still exists, and the subsequent
      // removal would then unregister the slot entirely — leaving the visible new
      // div unregistered. Unregistering first frees the id so the new div
      // registers cleanly and replays its buffered/current content.
      for (const mutation of mutations) {
        for (const node of mutation.removedNodes) {
          if (node.nodeType !== ELEMENT_NODE) continue;
          this._autoUnregisterTree(node);
        }
      }
      for (const mutation of mutations) {
        for (const node of mutation.addedNodes) {
          if (node.nodeType !== ELEMENT_NODE) continue;
          this._autoRegisterTree(node);
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
      // Free both the slot-level subscriptions and the rendered fragment's
      // store subscriptions — otherwise a panel rebuild leaks the old tree.
      this._clearContainerContent(entry);
      this._runCleanups(entry);
    }
    this._slots.clear();
    this._pendingContent.clear();
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
