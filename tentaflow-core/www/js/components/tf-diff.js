// =============================================================================
// File: tf-diff.js
// Description: <tf-diff> — unified / side-by-side diff viewer with per-hunk
//              review. Backs Code Studio's change review (§13.2): a patch set is
//              accepted hunk by hunk, and a file whose content moved under the
//              patch set renders a conflict block instead of a silent overwrite.
//              Light DOM, styles live in css/controls.css, no external deps.
//
//              A unified row carries two line-number columns (old, new) unless
//              `gutters` narrows it to one; a split pane carries the one column
//              of the side it renders.
//
//              Attributes: mode (unified|split), gutters (both|old|new),
//                reviewable, wrap, aria-label.
//              Properties: hunks, summary, conflict, mode, gutters, reviewable, wrap,
//                labels (i18n dict — English fallbacks only).
//              Methods : setHunkStatus(hunkId, status) — rewrites ONLY that hunk.
//              Events  : "hunk-decide" (cancelable; detail {hunkId, decision})
//                — decision is accept|reject|revert; calling preventDefault()
//                keeps the component fully controlled by the caller,
//                "line-click" (detail {hunkId, oldLn, newLn}).
//
// Example: const d = document.querySelector('tf-diff');
//          d.summary = { path: 'src/api.rs', added: 12, removed: 3, changeKind: 'modify' };
//          d.hunks = [{ id: 'h1', header: '@@ -1,4 +1,16 @@', status: 'pending',
//                       lines: [{ kind: 'add', oldLn: null, newLn: 2, text: 'use serde;' }] }];
//          d.addEventListener('hunk-decide', (e) => save(e.detail));
// =============================================================================

const MODES = new Set(['unified', 'split']);
// Which line-number columns a unified body carries. A diff needs both sides; a
// listing of ONE version (the reconciliation panes) needs only its own.
const GUTTERS = new Set(['both', 'old', 'new']);
const STATUSES = new Set(['pending', 'accepted', 'rejected', 'conflicted']);
const KINDS = new Set(['ctx', 'add', 'del']);

const STATUS_CLASS = {
  accepted: 'tf-diff__hunk--accepted',
  rejected: 'tf-diff__hunk--rejected',
  conflicted: 'tf-diff__hunk--conflicted',
};

const CHANGE_LABEL_KEY = {
  add: 'change_add', modify: 'change_modify', delete: 'change_delete', rename: 'change_rename',
};

const DEFAULT_LABELS = {
  diff: 'Diff',
  accept: 'Accept',
  reject: 'Reject',
  revert: 'Revert',
  state_accepted: 'accepted',
  state_rejected: 'rejected',
  state_conflicted: 'conflict',
  resolved: '{done}/{total} hunks resolved',
  empty: 'No changes',
  change_add: 'added',
  change_modify: 'modified',
  change_delete: 'deleted',
  change_rename: 'renamed',
  side_base: 'Base',
  side_result: 'Accepted result',
  conflict_title: 'The file changed after these edits were prepared',
  conflict_body: 'This patch set was built on content {base}; the file now holds {current}. '
    + 'Nothing is overwritten silently — decide what happens with the agent changes.',
};

function fmt(template, vars) {
  return String(template).replace(/\{(\w+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m));
}

// Fills a `{placeholder}` template into `parent`, where a value may be a DOM
// node. Keeps user text out of innerHTML while still allowing markup inside a
// translated sentence.
function fillTemplate(parent, template, vars) {
  const re = /\{(\w+)\}/g;
  const text = String(template);
  let last = 0;
  let m = re.exec(text);
  while (m) {
    if (m.index > last) parent.appendChild(document.createTextNode(text.slice(last, m.index)));
    const value = vars[m[1]];
    if (value instanceof Node) parent.appendChild(value);
    else parent.appendChild(document.createTextNode(value === undefined ? m[0] : String(value)));
    last = m.index + m[0].length;
    m = re.exec(text);
  }
  if (last < text.length) parent.appendChild(document.createTextNode(text.slice(last)));
}

function lineKind(line) {
  const kind = line?.kind;
  return KINDS.has(kind) ? kind : 'ctx';
}

function lineNo(value) {
  return Number.isInteger(value) && value > 0 ? String(value) : '';
}

class TfDiff extends HTMLElement {
  static get observedAttributes() { return ['mode', 'gutters', 'reviewable', 'wrap', 'aria-label']; }

  constructor() {
    super();
    this._root = null;
    this._headEl = null;
    this._conflictEl = null;
    this._listEl = null;
    this._countEl = null;

    this._hunks = [];
    this._summary = null;
    this._conflict = null;
    this._labels = { ...DEFAULT_LABELS };
    this._hunkEls = new Map();   // hunkId -> { section, actions, statusIndex }
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._render();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (!this._root || oldVal === newVal) return;
    if (name === 'aria-label') this._syncLabel();
    else if (name !== 'wrap') this._render();   // wrap is pure CSS
  }

  // ------------------------------------------------------------- public API

  get hunks() { return this._hunks; }
  set hunks(val) {
    this._hunks = (Array.isArray(val) ? val : []).map((h, i) => ({
      id: h?.id !== undefined && h?.id !== null ? String(h.id) : `hunk-${i}`,
      header: typeof h?.header === 'string' ? h.header : '',
      status: STATUSES.has(h?.status) ? h.status : 'pending',
      lines: Array.isArray(h?.lines) ? h.lines : [],
    }));
    if (this._root) this._render();
  }

  get summary() { return this._summary; }
  set summary(val) {
    this._summary = val && typeof val === 'object' ? val : null;
    if (this._root) this._render();
  }

  get conflict() { return this._conflict; }
  set conflict(val) {
    this._conflict = val && typeof val === 'object' ? val : null;
    if (this._root) this._render();
  }

  get mode() {
    const m = this.getAttribute('mode');
    return MODES.has(m) ? m : 'unified';
  }
  set mode(v) { this.setAttribute('mode', MODES.has(v) ? v : 'unified'); }

  get gutters() {
    const g = this.getAttribute('gutters');
    return GUTTERS.has(g) ? g : 'both';
  }
  set gutters(v) { this.setAttribute('gutters', GUTTERS.has(v) ? v : 'both'); }

  get reviewable() { return this.hasAttribute('reviewable'); }
  set reviewable(v) { if (v) this.setAttribute('reviewable', ''); else this.removeAttribute('reviewable'); }

  get wrap() { return this.hasAttribute('wrap'); }
  set wrap(v) { if (v) this.setAttribute('wrap', ''); else this.removeAttribute('wrap'); }

  get labels() { return this._labels; }
  set labels(dict) {
    this._labels = { ...DEFAULT_LABELS, ...(dict || {}) };
    this._syncLabel();
    if (this._root) this._render();
  }

  // Surgical state change: rewrites the one hunk's chrome, never the diff body
  // and never the other hunks. The resolved counter follows.
  setHunkStatus(hunkId, status) {
    const id = String(hunkId);
    const next = STATUSES.has(status) ? status : 'pending';
    const hunk = this._hunks.find((h) => h.id === id);
    if (!hunk) return false;
    hunk.status = next;
    const entry = this._hunkEls.get(id);
    if (entry) {
      for (const cls of Object.values(STATUS_CLASS)) entry.section.classList.remove(cls);
      if (STATUS_CLASS[next]) entry.section.classList.add(STATUS_CLASS[next]);
      entry.section.dataset.status = next;
      this._renderHunkActions(entry.actions, hunk);
    }
    this._renderCount();
    return true;
  }

  // ---------------------------------------------------------------- building

  _build() {
    this.innerHTML = '';
    const root = document.createElement('div');
    root.className = 'tf-diff';
    root.setAttribute('role', 'group');

    const head = document.createElement('div');
    head.className = 'tf-diff__head';

    const conflict = document.createElement('div');
    conflict.className = 'tf-diff__conflict';
    conflict.setAttribute('role', 'alert');
    conflict.hidden = true;

    const list = document.createElement('div');
    list.className = 'tf-diff__hunks';

    root.appendChild(head);
    root.appendChild(conflict);
    root.appendChild(list);
    this.appendChild(root);

    this._root = root;
    this._headEl = head;
    this._conflictEl = conflict;
    this._listEl = list;
    this._syncLabel();
  }

  _syncLabel() {
    this._root?.setAttribute('aria-label', this.getAttribute('aria-label') || this._labels.diff);
  }

  _render() {
    this._root.dataset.mode = this.mode;
    this._renderHead();
    this._renderConflict();
    this._renderHunks();
  }

  // ------------------------------------------------------------------- head

  _renderHead() {
    const head = this._headEl;
    head.textContent = '';
    const s = this._summary;
    if (!s) { head.hidden = true; this._countEl = null; return; }
    head.hidden = false;

    const path = document.createElement('span');
    path.className = 'tf-diff__path';
    path.textContent = s.path ? String(s.path) : '';
    head.appendChild(path);

    if (s.changeKind === 'rename' && s.oldPath) {
      const from = document.createElement('span');
      from.className = 'tf-diff__oldpath';
      from.textContent = String(s.oldPath);
      head.appendChild(from);
    }

    const kindKey = CHANGE_LABEL_KEY[s.changeKind];
    if (kindKey) {
      const kind = document.createElement('span');
      kind.className = `tf-diff__kind tf-diff__kind--${s.changeKind}`;
      kind.textContent = this._labels[kindKey];
      head.appendChild(kind);
    }

    if (Number.isInteger(s.added) && s.added > 0) {
      const added = document.createElement('span');
      added.className = 'tf-diff__stat tf-diff__stat--add';
      added.textContent = `+${s.added}`;
      head.appendChild(added);
    }
    if (Number.isInteger(s.removed) && s.removed > 0) {
      const removed = document.createElement('span');
      removed.className = 'tf-diff__stat tf-diff__stat--del';
      removed.textContent = `-${s.removed}`;
      head.appendChild(removed);
    }

    const spacer = document.createElement('span');
    spacer.className = 'tf-diff__spacer';
    head.appendChild(spacer);

    const count = document.createElement('span');
    count.className = 'tf-diff__count';
    head.appendChild(count);
    this._countEl = count;
    this._renderCount();
  }

  _renderCount() {
    const el = this._countEl;
    if (!el) return;
    if (!this.reviewable || !this._hunks.length) { el.textContent = ''; return; }
    const done = this._hunks.filter((h) => h.status !== 'pending').length;
    el.textContent = fmt(this._labels.resolved, { done, total: this._hunks.length });
  }

  // --------------------------------------------------------------- conflict

  // Both digests are shown: the one the patch set was built on and the one the
  // file carries now. Without them the user cannot tell what moved underneath.
  _renderConflict() {
    const el = this._conflictEl;
    el.textContent = '';
    const c = this._conflict;
    if (!c) { el.hidden = true; return; }
    el.hidden = false;

    const title = document.createElement('h4');
    title.className = 'tf-diff__conflict-title';
    title.textContent = this._labels.conflict_title;
    el.appendChild(title);

    const body = document.createElement('p');
    body.className = 'tf-diff__conflict-body';
    const base = document.createElement('code');
    base.className = 'tf-diff__sha';
    base.textContent = c.basedOnSha ? String(c.basedOnSha) : '';
    const current = document.createElement('code');
    current.className = 'tf-diff__sha';
    current.textContent = c.currentSha ? String(c.currentSha) : '';
    fillTemplate(body, this._labels.conflict_body, { base, current });
    el.appendChild(body);

    if (c.message) {
      const msg = document.createElement('p');
      msg.className = 'tf-diff__conflict-msg';
      msg.textContent = String(c.message);
      el.appendChild(msg);
    }
  }

  // ------------------------------------------------------------------ hunks

  _renderHunks() {
    const list = this._listEl;
    list.textContent = '';
    this._hunkEls.clear();

    if (!this._hunks.length) {
      const empty = document.createElement('div');
      empty.className = 'tf-diff__empty';
      empty.textContent = this._labels.empty;
      list.appendChild(empty);
      return;
    }

    const split = this.mode === 'split';
    for (const hunk of this._hunks) {
      const section = document.createElement('section');
      section.className = 'tf-diff__hunk';
      if (STATUS_CLASS[hunk.status]) section.classList.add(STATUS_CLASS[hunk.status]);
      section.dataset.hunkId = hunk.id;
      section.dataset.status = hunk.status;

      const head = document.createElement('header');
      head.className = 'tf-diff__hunk-head';

      const title = document.createElement('span');
      title.className = 'tf-diff__hunk-title';
      title.textContent = hunk.header;
      head.appendChild(title);

      const spacer = document.createElement('span');
      spacer.className = 'tf-diff__spacer';
      head.appendChild(spacer);

      const actions = document.createElement('span');
      actions.className = 'tf-diff__hunk-actions';
      head.appendChild(actions);
      this._renderHunkActions(actions, hunk);

      section.appendChild(head);
      section.appendChild(split ? this._buildSplit(hunk) : this._buildUnified(hunk));
      list.appendChild(section);
      this._hunkEls.set(hunk.id, { section, actions });
    }
    this._renderCount();
  }

  _renderHunkActions(container, hunk) {
    container.textContent = '';
    if (hunk.status !== 'pending') {
      const pill = document.createElement('span');
      pill.className = `tf-diff__state tf-diff__state--${hunk.status}`;
      pill.textContent = this._labels[`state_${hunk.status}`] || hunk.status;
      container.appendChild(pill);
    }
    if (!this.reviewable || hunk.status === 'conflicted') return;

    if (hunk.status === 'pending') {
      container.appendChild(this._actionButton(hunk.id, 'reject', this._labels.reject, 'tf-btn-danger'));
      container.appendChild(this._actionButton(hunk.id, 'accept', this._labels.accept, 'tf-btn-primary'));
    } else {
      container.appendChild(this._actionButton(hunk.id, 'revert', this._labels.revert, 'tf-btn-ghost'));
    }
  }

  _actionButton(hunkId, decision, label, variantClass) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = `tf-btn tf-btn-sm ${variantClass} tf-diff__action`;
    btn.dataset.decision = decision;
    btn.textContent = label;
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      this._decide(hunkId, decision);
    });
    return btn;
  }

  _decide(hunkId, decision) {
    const ev = new CustomEvent('hunk-decide', {
      bubbles: false,
      cancelable: true,
      detail: { hunkId, decision },
    });
    const proceed = this.dispatchEvent(ev);
    if (!proceed) return;   // caller keeps full control of the status
    const next = decision === 'accept' ? 'accepted' : (decision === 'reject' ? 'rejected' : 'pending');
    this.setHunkStatus(hunkId, next);
  }

  // A unified body interleaves both sides, so a single number column would have
  // to alternate between the old and the new numbering and would run backwards
  // wherever a deletion follows context. Each side keeps its own column, unless
  // the caller renders one version only (`gutters`).
  _buildUnified(hunk) {
    const body = document.createElement('div');
    body.className = 'tf-diff__body';
    const wanted = this.gutters;
    for (const line of hunk.lines) {
      const gutters = [];
      if (wanted !== 'new') gutters.push({ side: 'old', value: line.oldLn });
      if (wanted !== 'old') gutters.push({ side: 'new', value: line.newLn });
      body.appendChild(this._buildLine(hunk.id, line, gutters));
    }
    return body;
  }

  // Split renders two independent blocks — the base content and the content the
  // accepted hunks reconstruct — each with its own line-number column.
  _buildSplit(hunk) {
    const split = document.createElement('div');
    split.className = 'tf-diff__split';

    const sides = [
      { caption: this._labels.side_base, keep: new Set(['ctx', 'del']), side: 'old', num: (l) => l.oldLn, cls: 'tf-diff__pane--base' },
      { caption: this._labels.side_result, keep: new Set(['ctx', 'add']), side: 'new', num: (l) => l.newLn, cls: 'tf-diff__pane--result' },
    ];

    for (const side of sides) {
      const pane = document.createElement('div');
      pane.className = `tf-diff__pane ${side.cls}`;
      const caption = document.createElement('div');
      caption.className = 'tf-diff__pane-head';
      caption.textContent = side.caption;
      pane.appendChild(caption);
      const body = document.createElement('div');
      body.className = 'tf-diff__body';
      for (const line of hunk.lines) {
        if (!side.keep.has(lineKind(line))) continue;
        body.appendChild(this._buildLine(hunk.id, line, [{ side: side.side, value: side.num(line) }]));
      }
      pane.appendChild(body);
      split.appendChild(pane);
    }
    return split;
  }

  _buildLine(hunkId, line, gutters) {
    const kind = lineKind(line);
    const row = document.createElement('div');
    row.className = `tf-diff__line tf-diff__line--${kind}`;

    for (const gutter of gutters) {
      const num = document.createElement('span');
      num.className = `tf-diff__ln tf-diff__ln--${gutter.side}`;
      num.setAttribute('aria-hidden', 'true');
      num.textContent = lineNo(gutter.value);
      row.appendChild(num);
    }

    const text = document.createElement('span');
    text.className = 'tf-diff__text';
    text.textContent = typeof line?.text === 'string' ? line.text : '';
    row.appendChild(text);

    row.addEventListener('click', () => {
      this.dispatchEvent(new CustomEvent('line-click', {
        bubbles: false,
        detail: {
          hunkId,
          oldLn: Number.isInteger(line?.oldLn) ? line.oldLn : null,
          newLn: Number.isInteger(line?.newLn) ? line.newLn : null,
        },
      }));
    });
    return row;
  }
}

customElements.define('tf-diff', TfDiff);
export { TfDiff };
