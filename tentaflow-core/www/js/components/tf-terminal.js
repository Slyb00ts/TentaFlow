// =============================================================================
// File: tf-terminal.js
// Description: <tf-terminal> — terminal screen that renders a server-owned VT
//              cell grid and forwards key presses. The VT100 state machine runs
//              on the owner node (Code Studio §7.9): the browser receives a cell
//              grid carrying a revision number, never an escape-sequence stream.
//              The component therefore parses nothing — it renders and sends.
//              Light DOM, styles live in css/controls.css, no external deps.
//
//              Attributes: rows, cols (the grid the server currently keeps —
//                drives the minimum screen box), readonly, aria-label.
//              Properties: revision (read-only), applicationCursor,
//                bracketedPaste, labels (i18n dict, English fallbacks only).
//              Methods : applySnapshot(snapshot), applyChanges(patch),
//                scrollToBottom(), focus().
//              Events  : "key" (detail {bytes:Uint8Array}), "resize"
//                (detail {rows, cols} — the grid that fits the current box),
//                "resync" (detail {have, received} — a revision gap was seen and
//                nothing was applied; the caller must request a full snapshot).
//
// Example: const t = document.querySelector('tf-terminal');
//          t.applySnapshot({ revision: 1, cursor: { row: 0, col: 0, visible: true },
//                            rows: [[{ ch: '$', fg: 2, bg: null, attrs: 1 }]] });
//          t.addEventListener('key', (e) => pty.write(e.detail.bytes));
// =============================================================================

// Cell attribute bitmask. The server sends one integer per cell; the names
// mirror the SGR parameters they come from.
export const TERM_ATTR = Object.freeze({
  BOLD: 1,
  DIM: 2,
  ITALIC: 4,
  UNDERLINE: 8,
  BLINK: 16,
  REVERSE: 32,
  HIDDEN: 64,
  STRIKE: 128,
});

const ATTR_CLASS = [
  [TERM_ATTR.BOLD, 'tf-terminal__run--bold'],
  [TERM_ATTR.DIM, 'tf-terminal__run--dim'],
  [TERM_ATTR.ITALIC, 'tf-terminal__run--italic'],
  [TERM_ATTR.UNDERLINE, 'tf-terminal__run--underline'],
  [TERM_ATTR.BLINK, 'tf-terminal__run--blink'],
  [TERM_ATTR.HIDDEN, 'tf-terminal__run--hidden'],
  [TERM_ATTR.STRIKE, 'tf-terminal__run--strike'],
];

const RULER_CHARS = 100;

const DEFAULT_LABELS = {
  terminal: 'Terminal',
  input: 'Terminal input',
};

// Sentinels for the two default colours. Reverse video swaps fg/bg literally,
// so a defaulted colour has to survive the swap as a value.
const DEF_FG = 'def-fg';
const DEF_BG = 'def-bg';

// ---------------------------------------------------------------------------
// Colour resolution
// ---------------------------------------------------------------------------

function hex2(n) {
  const v = Math.max(0, Math.min(255, n | 0));
  return v.toString(16).padStart(2, '0');
}

function rgbHex(r, g, b) { return `#${hex2(r)}${hex2(g)}${hex2(b)}`; }

// xterm 256-colour palette above the 16 ANSI slots: a 6x6x6 cube then 24 greys.
function xtermCubeHex(index) {
  if (index < 232) {
    const n = index - 16;
    const level = (v) => (v ? 55 + v * 40 : 0);
    return rgbHex(level(Math.floor(n / 36)), level(Math.floor((n % 36) / 6)), level(n % 6));
  }
  const v = 8 + (index - 232) * 10;
  return rgbHex(v, v, v);
}

// Accepts: null/undefined (default), 0..255 palette index, '#rgb'/'#rrggbb',
// [r,g,b] or {r,g,b}. Anything else resolves to the default colour.
function normColor(spec, fallback) {
  if (spec === null || spec === undefined) return fallback;
  if (typeof spec === 'number') {
    if (!Number.isInteger(spec) || spec < 0 || spec > 255) return fallback;
    return spec < 16 ? spec : xtermCubeHex(spec);
  }
  if (typeof spec === 'string') {
    const s = spec.trim();
    if (/^#[0-9a-f]{6}$/i.test(s)) return s.toLowerCase();
    if (/^#[0-9a-f]{3}$/i.test(s)) return `#${s[1]}${s[1]}${s[2]}${s[2]}${s[3]}${s[3]}`.toLowerCase();
    if (/^[0-9a-f]{6}$/i.test(s)) return `#${s.toLowerCase()}`;
    return fallback;
  }
  if (Array.isArray(spec) && spec.length >= 3) return rgbHex(spec[0], spec[1], spec[2]);
  if (typeof spec === 'object' && 'r' in spec && 'g' in spec && 'b' in spec) {
    return rgbHex(spec.r, spec.g, spec.b);
  }
  return fallback;
}

// Per-cell style descriptor shared by the run grouping and the DOM writer.
function cellStyle(cell) {
  const attrs = Number.isInteger(cell?.attrs) ? cell.attrs : 0;
  let fg = normColor(cell?.fg, DEF_FG);
  let bg = normColor(cell?.bg, DEF_BG);
  if (attrs & TERM_ATTR.REVERSE) { const t = fg; fg = bg; bg = t; }
  return { fg, bg, attrs };
}

function styleKey(style) { return style.fg + '|' + style.bg + '|' + style.attrs; }

function isBlankCell(cell) {
  const st = cellStyle(cell);
  if (st.fg !== DEF_FG || st.bg !== DEF_BG || st.attrs !== 0) return false;
  const ch = cell?.ch;
  return ch === undefined || ch === null || ch === '' || ch === ' ';
}

function applyRunStyle(span, style) {
  const classes = ['tf-terminal__run'];
  if (style.fg === DEF_FG) { /* inherits the screen foreground */ }
  else if (style.fg === DEF_BG) classes.push('tf-terminal__run--fg-onbg');
  else if (typeof style.fg === 'number') classes.push(`tf-terminal__fg-${style.fg}`);
  else span.style.color = style.fg;

  if (style.bg === DEF_BG) { /* transparent, the screen shows through */ }
  else if (style.bg === DEF_FG) classes.push('tf-terminal__run--bg-onfg');
  else if (typeof style.bg === 'number') classes.push(`tf-terminal__bg-${style.bg}`);
  else span.style.backgroundColor = style.bg;

  for (const [bit, cls] of ATTR_CLASS) if (style.attrs & bit) classes.push(cls);
  span.className = classes.join(' ');
}

// ---------------------------------------------------------------------------
// Key encoding (xterm-256color)
// ---------------------------------------------------------------------------

const CSI_LETTER = {
  ArrowUp: 'A', ArrowDown: 'B', ArrowRight: 'C', ArrowLeft: 'D', Home: 'H', End: 'F',
};
const TILDE_CODE = {
  Insert: 2, Delete: 3, PageUp: 5, PageDown: 6,
  F5: 15, F6: 17, F7: 18, F8: 19, F9: 20, F10: 21, F11: 23, F12: 24,
};
const SS3_FN = { F1: 'P', F2: 'Q', F3: 'R', F4: 'S' };
const DEAD_KEYS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'CapsLock', 'NumLock',
  'ScrollLock', 'Dead', 'Process', 'Unidentified', 'AltGraph', 'ContextMenu']);

const CTRL_PUNCT = { ' ': 0, '@': 0, '[': 27, '\\': 28, ']': 29, '^': 30, '_': 31, '?': 127 };
const CTRL_DIGIT = { 2: 0, 3: 27, 4: 28, 5: 29, 6: 30, 7: 31, 8: 127 };

const encoder = new TextEncoder();

function bytesOf(text) { return encoder.encode(text); }

function modParam(ev) {
  return 1 + (ev.shiftKey ? 1 : 0) + (ev.altKey ? 2 : 0) + (ev.ctrlKey ? 4 : 0);
}

function ctrlByte(key) {
  if (key.length !== 1) return null;
  const lower = key.toLowerCase();
  const code = lower.charCodeAt(0);
  if (code >= 97 && code <= 122) return code - 96;      // Ctrl+a..z -> 0x01..0x1a
  if (lower in CTRL_PUNCT) return CTRL_PUNCT[lower];
  if (lower in CTRL_DIGIT) return CTRL_DIGIT[lower];
  return null;
}

// Encodes a keydown into the byte string a PTY in xterm-256color expects.
// `modes` mirrors the server-side VT modes the component cannot know by itself
// (DECCKM). Returns null when the key produces nothing.
export function encodeKeyEvent(ev, modes = {}) {
  const key = ev.key;
  if (!key || DEAD_KEYS.has(key)) return null;
  if (ev.isComposing) return null;
  if (ev.metaKey) return null;                          // leave OS shortcuts alone

  const mod = modParam(ev);
  const esc = ev.altKey ? '\x1b' : '';

  const letter = CSI_LETTER[key];
  if (letter) {
    if (mod === 1) return bytesOf(modes.applicationCursor ? `\x1bO${letter}` : `\x1b[${letter}`);
    return bytesOf(`\x1b[1;${mod}${letter}`);
  }

  const tilde = TILDE_CODE[key];
  if (tilde !== undefined) {
    return bytesOf(mod === 1 ? `\x1b[${tilde}~` : `\x1b[${tilde};${mod}~`);
  }

  const fn = SS3_FN[key];
  if (fn) return bytesOf(mod === 1 ? `\x1bO${fn}` : `\x1b[1;${mod}${fn}`);

  if (key === 'Enter') return bytesOf(`${esc}\r`);
  if (key === 'Tab') return bytesOf(ev.shiftKey ? '\x1b[Z' : `${esc}\t`);
  if (key === 'Backspace') return bytesOf(`${esc}${ev.ctrlKey ? '\x08' : '\x7f'}`);
  if (key === 'Escape') return bytesOf('\x1b');

  if (ev.ctrlKey) {
    const b = ctrlByte(key);
    if (b === null) return null;
    return ev.altKey ? new Uint8Array([0x1b, b]) : new Uint8Array([b]);
  }

  if ([...key].length === 1) return bytesOf(esc + key);
  return null;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

class TfTerminal extends HTMLElement {
  static get observedAttributes() { return ['rows', 'cols', 'readonly', 'aria-label']; }

  constructor() {
    super();
    this._root = null;
    this._view = null;
    this._screen = null;
    this._ruler = null;
    this._input = null;
    this._cursorEl = null;

    this._rowEls = [];
    this._revision = null;
    this._cursor = { row: 0, col: 0, visible: true };
    this._modes = { applicationCursor: false, bracketedPaste: false };
    this._labels = { ...DEFAULT_LABELS };

    this._charW = 0;
    this._rowH = 0;
    this._lastEmitted = { rows: 0, cols: 0 };
    this._ro = null;
    this._measureScheduled = false;
  }

  // ------------------------------------------------------------------ setup

  connectedCallback() {
    if (!this._root) this._build();
    this._syncGridVars();
    this._scheduleMeasure();
    if (typeof ResizeObserver === 'function') {
      this._ro = new ResizeObserver(() => this._scheduleMeasure());
      this._ro.observe(this._view);
    }
  }

  disconnectedCallback() {
    this._ro?.disconnect();
    this._ro = null;
  }

  attributeChangedCallback(name) {
    if (!this._root) return;
    if (name === 'rows' || name === 'cols') this._syncGridVars();
    else if (name === 'aria-label') this._syncLabels();
  }

  _build() {
    this.innerHTML = '';

    const root = document.createElement('div');
    root.className = 'tf-terminal';

    const view = document.createElement('div');
    view.className = 'tf-terminal__view';
    view.tabIndex = 0;
    view.setAttribute('role', 'application');

    const screen = document.createElement('div');
    screen.className = 'tf-terminal__screen';

    const cursor = document.createElement('div');
    cursor.className = 'tf-terminal__cursor';
    cursor.setAttribute('aria-hidden', 'true');
    screen.appendChild(cursor);

    const ruler = document.createElement('span');
    ruler.className = 'tf-terminal__ruler';
    ruler.setAttribute('aria-hidden', 'true');
    ruler.textContent = 'M'.repeat(RULER_CHARS);
    screen.appendChild(ruler);

    // Hidden textarea: the accessible/editable surface. It is what makes paste,
    // IME composition and native focus work on a grid made of plain <div>s.
    const input = document.createElement('textarea');
    input.className = 'tf-terminal__input';
    input.setAttribute('autocapitalize', 'off');
    input.setAttribute('autocorrect', 'off');
    input.setAttribute('autocomplete', 'off');
    input.spellcheck = false;
    input.wrap = 'off';

    view.appendChild(screen);
    view.appendChild(input);
    root.appendChild(view);
    this.appendChild(root);

    this._root = root;
    this._view = view;
    this._screen = screen;
    this._ruler = ruler;
    this._input = input;
    this._cursorEl = cursor;
    this._syncLabels();

    view.addEventListener('mouseup', () => {
      // Focusing on mouseup keeps a drag-selection intact; a plain click focuses.
      const sel = this.ownerDocument?.getSelection?.();
      if (!sel || sel.isCollapsed) this._input.focus({ preventScroll: true });
    });
    view.addEventListener('focus', () => this._input.focus({ preventScroll: true }));
    view.addEventListener('scroll', () => this._syncStickClass(), { passive: true });

    input.addEventListener('focus', () => root.classList.add('is-focused'));
    input.addEventListener('blur', () => root.classList.remove('is-focused'));
    input.addEventListener('keydown', (e) => this._onKeyDown(e));
    input.addEventListener('paste', (e) => this._onPaste(e));
    input.addEventListener('compositionend', (e) => {
      if (e.data) this._send(bytesOf(e.data));
      input.value = '';
    });
    input.addEventListener('input', () => { input.value = ''; });
  }

  _syncLabels() {
    const label = this.getAttribute('aria-label') || this._labels.terminal;
    this._view?.setAttribute('aria-label', label);
    this._input?.setAttribute('aria-label', this._labels.input);
  }

  _syncGridVars() {
    const cols = parseInt(this.getAttribute('cols') || '', 10);
    const rows = parseInt(this.getAttribute('rows') || '', 10);
    if (Number.isInteger(cols) && cols > 0) this.style.setProperty('--tf-term-cols', String(cols));
    else this.style.removeProperty('--tf-term-cols');
    if (Number.isInteger(rows) && rows > 0) this.style.setProperty('--tf-term-rows', String(rows));
    else this.style.removeProperty('--tf-term-rows');
  }

  // ------------------------------------------------------------- public API

  get revision() { return this._revision; }

  get readOnly() { return this.hasAttribute('readonly'); }
  set readOnly(v) { if (v) this.setAttribute('readonly', ''); else this.removeAttribute('readonly'); }

  get applicationCursor() { return this._modes.applicationCursor; }
  set applicationCursor(v) { this._modes.applicationCursor = !!v; }

  get bracketedPaste() { return this._modes.bracketedPaste; }
  set bracketedPaste(v) { this._modes.bracketedPaste = !!v; }

  get labels() { return this._labels; }
  set labels(dict) {
    this._labels = { ...DEFAULT_LABELS, ...(dict || {}) };
    this._syncLabels();
  }

  focus() {
    if (!this._root) this._build();
    this._input.focus({ preventScroll: true });
  }

  // Full grid replacement. A snapshot older than what is already on screen is
  // dropped — a late reply must never rewind the terminal.
  applySnapshot(snapshot) {
    if (!snapshot || typeof snapshot !== 'object') return false;
    const revision = Number(snapshot.revision);
    if (!Number.isFinite(revision)) return false;
    if (this._revision !== null && revision < this._revision) return false;
    if (!this._root) this._build();

    const rows = Array.isArray(snapshot.rows) ? snapshot.rows : [];
    const anchor = this._captureScroll();

    this._ensureRowCount(rows.length);
    for (let i = 0; i < rows.length; i++) this._writeRow(i, rows[i]);

    this._revision = revision;
    this._applyModes(snapshot.modes);
    this._setCursor(snapshot.cursor);
    this._restoreScroll(anchor);
    return true;
  }

  // Incremental update: only the listed row indices are rewritten. The revision
  // must be exactly one past the current one — a gap means rows we never saw,
  // so nothing is applied and the caller is told to ask for a snapshot.
  applyChanges(patch) {
    if (!patch || typeof patch !== 'object') return false;
    const revision = Number(patch.revision);
    if (!Number.isFinite(revision)) return false;
    if (this._revision === null) {
      this._emitResync(revision);
      return false;
    }
    if (revision <= this._revision) return false;
    if (revision !== this._revision + 1) {
      this._emitResync(revision);
      return false;
    }
    if (!this._root) this._build();

    const rows = Array.isArray(patch.rows) ? patch.rows : [];
    const anchor = this._captureScroll();
    let maxIndex = this._rowEls.length - 1;
    for (const row of rows) {
      const index = Number(row?.index);
      if (!Number.isInteger(index) || index < 0) continue;
      if (index > maxIndex) maxIndex = index;
    }
    this._ensureRowCount(maxIndex + 1);
    for (const row of rows) {
      const index = Number(row?.index);
      if (!Number.isInteger(index) || index < 0) continue;
      this._writeRow(index, row.cells);
    }

    this._revision = revision;
    this._applyModes(patch.modes);
    if (patch.cursor) this._setCursor(patch.cursor);
    this._restoreScroll(anchor);
    return true;
  }

  scrollToBottom() {
    if (!this._view) return;
    this._view.scrollTop = this._view.scrollHeight;
    this._syncStickClass();
  }

  // ---------------------------------------------------------------- grid DOM

  _ensureRowCount(count) {
    const els = this._rowEls;
    while (els.length < count) {
      const row = document.createElement('div');
      row.className = 'tf-terminal__row';
      this._screen.insertBefore(row, this._cursorEl);
      els.push(row);
    }
    while (els.length > count) els.pop().remove();
  }

  _writeRow(index, cells) {
    const rowEl = this._rowEls[index];
    if (!rowEl) return;
    rowEl.textContent = '';
    const list = Array.isArray(cells) ? cells : [];

    // Trailing blank cells carry no information and would pollute a copy.
    let end = list.length;
    while (end > 0 && isBlankCell(list[end - 1])) end--;
    if (end === 0) return;

    let i = 0;
    while (i < end) {
      const style = cellStyle(list[i]);
      const key = styleKey(style);
      let text = '';
      let j = i;
      while (j < end) {
        const st = cellStyle(list[j]);
        if (styleKey(st) !== key) break;
        const ch = list[j]?.ch;
        text += (ch === undefined || ch === null || ch === '') ? ' ' : String(ch);
        j++;
      }
      const span = document.createElement('span');
      applyRunStyle(span, style);
      span.textContent = text;
      rowEl.appendChild(span);
      i = j;
    }
  }

  _applyModes(modes) {
    if (!modes || typeof modes !== 'object') return;
    if ('applicationCursor' in modes) this._modes.applicationCursor = !!modes.applicationCursor;
    if ('bracketedPaste' in modes) this._modes.bracketedPaste = !!modes.bracketedPaste;
  }

  _setCursor(cursor) {
    if (cursor && typeof cursor === 'object') {
      this._cursor = {
        row: Number.isInteger(cursor.row) ? cursor.row : 0,
        col: Number.isInteger(cursor.col) ? cursor.col : 0,
        visible: cursor.visible !== false,
      };
    }
    this._placeCursor();
  }

  // cursor.row/col index the delivered grid (scrollback included), not the
  // viewport — the server owns the coordinate system, the browser just paints.
  _placeCursor() {
    const el = this._cursorEl;
    if (!el) return;
    if (!this._cursor.visible) {
      el.hidden = true;
      return;
    }
    el.hidden = false;
    this._measureCell();
    const x = this._cursor.col * this._charW;
    const y = this._cursor.row * this._rowH;
    el.style.transform = `translate(${x}px, ${y}px)`;
  }

  _emitResync(received) {
    this.dispatchEvent(new CustomEvent('resync', {
      bubbles: false,
      detail: { have: this._revision, received },
    }));
  }

  // --------------------------------------------------------------- scrolling

  _isAtBottom() {
    const v = this._view;
    if (!v) return true;
    return v.scrollHeight - v.scrollTop - v.clientHeight <= 2;
  }

  _captureScroll() {
    const v = this._view;
    if (!v) return { bottom: true, dist: 0 };
    return { bottom: this._isAtBottom(), dist: v.scrollHeight - v.scrollTop };
  }

  // Sticks to the bottom while the user is already there; otherwise keeps the
  // visible slice pinned even when scrollback is trimmed or prepended.
  _restoreScroll(anchor) {
    const v = this._view;
    if (!v) return;
    if (anchor.bottom) v.scrollTop = v.scrollHeight;
    else v.scrollTop = Math.max(0, v.scrollHeight - anchor.dist);
    this._syncStickClass();
  }

  _syncStickClass() {
    this._root?.classList.toggle('is-scrolled-back', !this._isAtBottom());
  }

  // ------------------------------------------------------------- measurement

  _measureCell() {
    if (!this._ruler) return;
    const rect = this._ruler.getBoundingClientRect?.();
    if (rect && rect.width > 0) this._charW = rect.width / RULER_CHARS;
    const h = this._ruler.offsetHeight || rect?.height || 0;
    if (h > 0) this._rowH = h;
  }

  _scheduleMeasure() {
    if (this._measureScheduled) return;
    this._measureScheduled = true;
    const run = () => { this._measureScheduled = false; this._measureFit(); };
    if (typeof requestAnimationFrame === 'function') requestAnimationFrame(run);
    else run();
  }

  // Reports the grid that fits the current box. The rows/cols attributes stay
  // owner-controlled: the server resizes the PTY and sends the next snapshot.
  _measureFit() {
    if (!this._view || !this.isConnected) return;
    this._measureCell();
    this._placeCursor();
    if (!this._charW || !this._rowH) return;
    const cols = Math.max(1, Math.floor(this._screen.clientWidth / this._charW));
    const rows = Math.max(1, Math.floor(this._view.clientHeight / this._rowH));
    if (!Number.isFinite(cols) || !Number.isFinite(rows)) return;
    if (cols === this._lastEmitted.cols && rows === this._lastEmitted.rows) return;
    this._lastEmitted = { rows, cols };
    this.dispatchEvent(new CustomEvent('resize', { bubbles: false, detail: { rows, cols } }));
  }

  // ---------------------------------------------------------------- keyboard

  _send(bytes) {
    if (!bytes || !bytes.length) return;
    this.dispatchEvent(new CustomEvent('key', { bubbles: false, detail: { bytes } }));
  }

  _onKeyDown(ev) {
    if (this.readOnly) return;
    // Copy/paste and the browser's own find stay with the browser.
    if ((ev.ctrlKey || ev.metaKey) && ev.shiftKey && (ev.key === 'C' || ev.key === 'c')) return;
    if (ev.metaKey) return;

    const bytes = encodeKeyEvent(ev, this._modes);
    if (!bytes) return;
    ev.preventDefault();
    this._send(bytes);
    this.scrollToBottom();
  }

  _onPaste(ev) {
    if (this.readOnly) return;
    const text = ev.clipboardData?.getData('text') ?? '';
    ev.preventDefault();
    if (!text) return;
    const payload = this._modes.bracketedPaste
      ? `\x1b[200~${text.replace(/\x1b\[201~/g, '')}\x1b[201~`
      : text.replace(/\r\n/g, '\r').replace(/\n/g, '\r');
    this._send(bytesOf(payload));
    this.scrollToBottom();
  }
}

customElements.define('tf-terminal', TfTerminal);
export { TfTerminal };
