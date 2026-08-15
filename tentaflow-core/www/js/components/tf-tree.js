// =============================================================================
// File: tf-tree.js
// Description: <tf-tree> — hierarchical tree with chevron expand/collapse,
//              selectable rows, disabled nodes, DOM-node icons, status badges,
//              lazy children and roving-tabindex keyboard navigation. Controlled
//              component: expansion/selection state comes in via
//              `expandedIds`/`selectedId` properties; user intent is emitted as
//              `expand`/`collapse`/`select` events (non-bubbling, detail
//              `{ id }`, expand adds `lazy`). Light DOM.
//
//              Node shape: { id, label, children?, hasChildren?, disabled?,
//                icon? (a DOM element, rendered BEFORE the label),
//                badge? (a short status marker rendered AFTER the label and
//                  pushed to the row's right edge — either a string, or
//                  { text, tone } with tone a|m|d|c = added/modified/deleted/
//                  conflict) }
// Example: const tree = document.querySelector('tf-tree');
//          tree.nodes = [{id:'a', label:'A', children:[{id:'a1', label:'A1'}]}];
//          tree.expandedIds = ['a'];
//          tree.addEventListener('select', e => console.log(e.detail.id));
// =============================================================================

const VARIANTS = new Set(['default', 'compact', 'with_icons']);
// Status tones for `node.badge`: added / modified / deleted / conflict.
const BADGE_TONES = new Set(['a', 'm', 'd', 'c']);
// Defensive cap: cyclic or absurdly deep node graphs stop rendering instead of
// recursing forever. Callers needing strict validation enforce depth themselves.
const MAX_DEPTH = 32;

// Accepts a bare string or { text|label, tone }. Anything else yields no badge.
function normalizeBadge(badge) {
  if (badge == null) return null;
  if (typeof badge === 'string' || typeof badge === 'number') {
    const text = String(badge);
    return text ? { text, tone: '' } : null;
  }
  if (typeof badge !== 'object') return null;
  const raw = badge.text != null ? badge.text : badge.label;
  const text = raw == null ? '' : String(raw);
  if (!text) return null;
  const tone = typeof badge.tone === 'string' ? badge.tone.toLowerCase() : '';
  return { text, tone: BADGE_TONES.has(tone) ? tone : '' };
}

class TfTree extends HTMLElement {
  static get observedAttributes() {
    return ['variant', 'lazy'];
  }

  constructor() {
    super();
    this._container = null;
    this._rootList = null;
    this._nodes = [];
    this._expandedIds = new Set();
    this._selectedId = null;
    this._flatVisible = [];
    this._nodeElements = new Map();
  }

  connectedCallback() {
    this._ensureBuilt();
    this._render();
  }

  attributeChangedCallback() {
    if (this._container) this._render();
  }

  set nodes(val) {
    this._nodes = Array.isArray(val) ? val : [];
    this._ensureBuilt();
    this._render();
  }

  get nodes() {
    return this._nodes;
  }

  set expandedIds(val) {
    const ids = val instanceof Set ? [...val] : (Array.isArray(val) ? val : []);
    this._expandedIds = new Set(ids.filter((s) => typeof s === 'string'));
    this._ensureBuilt();
    this._render();
  }

  get expandedIds() {
    return this._expandedIds;
  }

  set selectedId(val) {
    this._selectedId = typeof val === 'string' ? val : null;
    this._ensureBuilt();
    this._render();
  }

  get selectedId() {
    return this._selectedId;
  }

  _isLazy() {
    return this.hasAttribute('lazy');
  }

  _ensureBuilt() {
    if (this._container) return;
    this.innerHTML = '';
    const el = document.createElement('div');
    el.setAttribute('role', 'tree');
    el.addEventListener('click', (e) => this._onClick(e));
    el.addEventListener('keydown', (e) => this._onKeydown(e));
    const root = document.createElement('ul');
    root.classList.add('tf-tree__root');
    root.setAttribute('role', 'group');
    el.appendChild(root);
    this.appendChild(el);
    this._container = el;
    this._rootList = root;
  }

  _emit(kind, detail) {
    this.dispatchEvent(new CustomEvent(kind, { bubbles: false, detail }));
  }

  _hasChildren(node) {
    return (Array.isArray(node.children) && node.children.length > 0)
      || (this._isLazy() && node.hasChildren === true);
  }

  _render() {
    const variantAttr = this.getAttribute('variant');
    const variant = VARIANTS.has(variantAttr) ? variantAttr : 'default';
    this._container.className =
      `tf-tree tf-tree--variant-${variant}${this._isLazy() ? ' tf-tree--lazy' : ''}`;

    const focusedId = this._captureFocusedId();
    this._rootList.replaceChildren();
    this._nodeElements = new Map();
    this._flatVisible = [];
    for (const node of this._nodes) {
      const li = this._renderNode(node, 0);
      if (li) this._rootList.appendChild(li);
    }
    if (focusedId != null) this._restoreFocus(focusedId);
  }

  _renderNode(node, depth) {
    if (!node || typeof node !== 'object' || typeof node.id !== 'string') return null;
    if (depth > MAX_DEPTH) return null;

    const li = document.createElement('li');
    li.classList.add('tf-tree__node');
    li.setAttribute('role', 'treeitem');
    li.setAttribute('data-node-id', node.id);
    li.setAttribute('data-depth', String(depth));
    const disabled = node.disabled === true;
    if (disabled) {
      li.classList.add('tf-tree__node--disabled');
      li.setAttribute('aria-disabled', 'true');
    }
    this._nodeElements.set(node.id, li);

    const row = document.createElement('div');
    row.classList.add('tf-tree__row');
    row.style.paddingLeft = `${depth * 1.25}em`;
    row.setAttribute('tabindex', '-1');

    const expanded = this._expandedIds.has(node.id);
    const hasChildren = this._hasChildren(node);

    const caret = document.createElement('span');
    caret.classList.add('tf-tree__caret');
    caret.setAttribute('aria-hidden', 'true');
    if (hasChildren) {
      caret.textContent = expanded ? '▾' : '▸';
      caret.classList.add('tf-tree__caret--clickable');
    } else {
      caret.textContent = ' ';
      caret.classList.add('tf-tree__caret--empty');
    }
    row.appendChild(caret);

    if (node.icon != null && typeof node.icon === 'object' && node.icon.nodeType === 1) {
      node.icon.classList.add('tf-tree__icon');
      row.appendChild(node.icon);
    }

    const label = document.createElement('span');
    label.classList.add('tf-tree__label');
    label.textContent = node.label != null ? String(node.label) : node.id;
    row.appendChild(label);

    // The badge follows the label so a file name reads first and its status
    // marker sits at the row's right edge.
    const badge = normalizeBadge(node.badge);
    if (badge) {
      const badgeEl = document.createElement('span');
      badgeEl.classList.add('tf-tree__badge');
      if (badge.tone) badgeEl.classList.add(`tf-tree__badge--${badge.tone}`);
      badgeEl.textContent = badge.text;
      row.appendChild(badgeEl);
    }

    if (this._selectedId === node.id) {
      li.classList.add('tf-tree__node--selected');
      li.setAttribute('aria-selected', 'true');
    }
    if (hasChildren) {
      li.setAttribute('aria-expanded', expanded ? 'true' : 'false');
    }

    li.appendChild(row);
    this._flatVisible.push({ id: node.id, depth, hasChildren, expanded, disabled });

    if (hasChildren && expanded && Array.isArray(node.children) && node.children.length > 0) {
      const childList = document.createElement('ul');
      childList.classList.add('tf-tree__children');
      childList.setAttribute('role', 'group');
      for (const child of node.children) {
        const childLi = this._renderNode(child, depth + 1);
        if (childLi) childList.appendChild(childLi);
      }
      li.appendChild(childList);
    }
    return li;
  }

  _onClick(e) {
    const row = e.target.closest('.tf-tree__row');
    if (!row || !this._container.contains(row)) return;
    const li = row.parentElement;
    const id = li && li.getAttribute('data-node-id');
    if (id == null) return;
    const meta = this._flatVisible.find((n) => n.id === id);
    if (!meta) return;
    e.preventDefault();
    if (meta.disabled) return;
    const caret = row.querySelector('.tf-tree__caret');
    if (caret && (e.target === caret || caret.contains(e.target))) {
      if (!meta.hasChildren) return;
      e.stopPropagation();
      if (meta.expanded) this._emit('collapse', { id });
      else this._emit('expand', { id, lazy: this._isLazy() });
      return;
    }
    this._emit('select', { id });
  }

  _onKeydown(e) {
    const active = document.activeElement;
    const activeRow = active && active.closest ? active.closest('.tf-tree__row') : null;
    if (!activeRow || !this._container.contains(activeRow)) return;
    const activeLi = activeRow.parentElement;
    const id = activeLi && activeLi.getAttribute('data-node-id');
    if (id == null) return;
    const flat = this._flatVisible;
    const idx = flat.findIndex((n) => n.id === id);
    const cur = flat[idx];
    if (!cur) return;
    switch (e.key) {
      case 'ArrowDown': {
        e.preventDefault();
        if (idx + 1 < flat.length) this._focusNode(flat[idx + 1].id);
        return;
      }
      case 'ArrowUp': {
        e.preventDefault();
        if (idx > 0) this._focusNode(flat[idx - 1].id);
        return;
      }
      case 'ArrowRight': {
        if (!cur.hasChildren) return;
        e.preventDefault();
        if (cur.disabled) return;
        if (!cur.expanded) this._emit('expand', { id: cur.id, lazy: this._isLazy() });
        else if (idx + 1 < flat.length && flat[idx + 1].depth > cur.depth) {
          this._focusNode(flat[idx + 1].id);
        }
        return;
      }
      case 'ArrowLeft': {
        e.preventDefault();
        if (cur.expanded) {
          if (cur.disabled) return;
          this._emit('collapse', { id: cur.id });
        } else if (cur.depth > 0) {
          for (let i = idx - 1; i >= 0; i--) {
            if (flat[i].depth < cur.depth) { this._focusNode(flat[i].id); break; }
          }
        }
        return;
      }
      case 'Enter':
      case ' ': {
        e.preventDefault();
        if (!cur.disabled) this._emit('select', { id: cur.id });
        return;
      }
      case 'Home': {
        e.preventDefault();
        if (flat.length > 0) this._focusNode(flat[0].id);
        return;
      }
      case 'End': {
        e.preventDefault();
        if (flat.length > 0) this._focusNode(flat[flat.length - 1].id);
        return;
      }
    }
  }

  _captureFocusedId() {
    const active = document.activeElement;
    if (!active || !this._container || !this._container.contains(active)) return null;
    const row = active.closest ? active.closest('.tf-tree__row') : null;
    if (!row) return null;
    const li = row.parentElement;
    return li && li.hasAttribute('data-node-id') ? li.getAttribute('data-node-id') : null;
  }

  _restoreFocus(id) {
    const li = this._nodeElements.get(id);
    if (!li) return;
    const row = li.querySelector('.tf-tree__row');
    if (row) try { row.focus(); } catch { /* detached element */ }
  }

  _focusNode(id) {
    const li = this._nodeElements.get(id);
    if (!li) return;
    const row = li.querySelector('.tf-tree__row');
    if (!row) return;
    row.setAttribute('tabindex', '0');
    for (const el of this._container.querySelectorAll('.tf-tree__row')) {
      if (el !== row) el.setAttribute('tabindex', '-1');
    }
    try { row.focus(); } catch { /* detached element */ }
  }
}

customElements.define('tf-tree', TfTree);
export { TfTree };
