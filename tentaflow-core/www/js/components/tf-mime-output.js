// =============================================================================
// File: components/tf-mime-output.js
// Description: <tf-mime-output> — renders one mime bundle (plan §13.2; mockups
//              Q06/Q15). A notebook cell, a run artifact and a kata check all
//              hand back the same shape a Jupyter kernel does — {mime: value} —
//              and this is the single place the dashboard turns that into DOM.
//
//              Only the RICHEST representation is drawn, the way a notebook
//              front end does: MIME_PREFERENCE decides, `preferred` overrides.
//
//              Standard types: text/plain, text/markdown (the dashboard's own
//              renderer), sanitised text/html, image/png|jpeg,
//              application/json (a tf-tree), text/x-traceback.
//              TentaQuant types (plan §4.3 — the suffix names the payload's
//              serialisation, and these strings are the SAME ones the T1
//              executor writes into `cell_outputs`, so a browser run and a
//              node run of one circuit land in this element identically):
//              -counts+json (a histogram with overlaid series), -state+json
//              (a Bloch row plus the amplitude table with phase colours),
//              -probs+json (the exact distribution of the same state, as a
//              histogram) and -circuit+json (a read-only tf-quantum-circuit).
//
//              The -state payload is a keyframe as the simulator serialises it
//              (`{step, gate, bloch: [[x,y,z], ...], purity, pairs, top,
//              probsTop}`); `bloch` flattened and `amplitudes` interleaved are
//              accepted too, because both the stepping API and the T1 state
//              artifact return those.
//
//              Long text collapses behind a "show all" button rather than
//              pushing the rest of a notebook off the screen.
//
//  Properties: bundle   — {mime: value},
//              preferred — mime types to try before MIME_PREFERENCE,
//              labels   — i18n dict, English fallbacks only.
//  Attributes: max-lines (default 40), max-rows (default 32); aria-label is
//              read once and defaults to labels.output.
//  Events    : "expand" detail {mime} when the reader opens a collapsed output.
//
// Example: const out = document.querySelector('tf-mime-output');
//          out.bundle = { 'application/x-tentaquant-counts+json': { shots: 1024,
//            counts: { '00': 517, '11': 507 } } };
// =============================================================================

import './tf-tree.js';
import './tf-bar-chart.js';
import { blochVectorList } from './tf-bloch-sphere.js';
import './tf-quantum-circuit.js';

export const COUNTS_MIME = 'application/x-tentaquant-counts+json';
export const STATE_MIME = 'application/x-tentaquant-state+json';
export const PROBS_MIME = 'application/x-tentaquant-probs+json';
export const CIRCUIT_MIME = 'application/x-tentaquant-circuit+json';
export const TRACEBACK_MIME = 'text/x-traceback';

/// Richest first. A bundle usually carries text/plain as the last resort, so it
/// sits at the bottom and every renderer above it wins when present.
///
/// A run's recorded evolution (`application/x-tentaquant-keyframes+cbor`) is
/// deliberately NOT here: its value is a `{sha256, size_bytes}` reference to a
/// CBOR blob in the content store, so there is nothing to draw and preferring
/// it would hide the histogram standing next to it in the same bundle.
export const MIME_PREFERENCE = [
  CIRCUIT_MIME,
  STATE_MIME,
  COUNTS_MIME,
  PROBS_MIME,
  TRACEBACK_MIME,
  'image/png',
  'image/jpeg',
  'text/html',
  'text/markdown',
  'application/json',
  'text/plain',
];

const DEFAULT_LABELS = {
  output: 'Output',
  show_all: 'Show all',
  unsupported: 'No renderer for this output',
  empty: 'No output',
  shots: 'shots',
  basis: 'basis state',
  amplitude: 'amplitude',
  probability: 'probability',
  phase: 'phase',
  ideal: 'ideal',
};

const DEFAULT_MAX_LINES = 40;
const DEFAULT_MAX_ROWS = 32;
const COUNTS_CHART_HEIGHT = 220;

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// The mime type this bundle should be drawn as, or null when nothing in it is
/// renderable.
export function pickMimeType(bundle, preferred) {
  if (!bundle || typeof bundle !== 'object') return null;
  const order = [...(Array.isArray(preferred) ? preferred : []), ...MIME_PREFERENCE];
  for (const mime of order) {
    if (Object.prototype.hasOwnProperty.call(bundle, mime) && bundle[mime] != null) return mime;
  }
  return null;
}

// ---------------------------------------------------------------------------
// HTML sanitiser. An output can carry HTML a kernel produced from arbitrary
// user code, so it is rebuilt node by node against an allowlist instead of
// being filtered by regex: anything not named here never reaches the document.
// ---------------------------------------------------------------------------

export const HTML_ALLOWED_TAGS = new Set([
  'a', 'abbr', 'b', 'blockquote', 'br', 'caption', 'code', 'col', 'colgroup', 'dd', 'div', 'dl',
  'dt', 'em', 'figcaption', 'figure', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'hr', 'i', 'img', 'li',
  'ol', 'p', 'pre', 's', 'small', 'span', 'strong', 'sub', 'sup', 'table', 'tbody', 'td', 'tfoot',
  'th', 'thead', 'tr', 'u', 'ul',
]);

/// Dropped WITH their subtree: their content is code, not text.
export const HTML_DROPPED_TAGS = new Set([
  'script', 'style', 'iframe', 'object', 'embed', 'link', 'meta', 'base', 'form', 'input',
  'button', 'select', 'textarea', 'template', 'svg', 'math', 'audio', 'video', 'canvas',
]);

// `class` is deliberately absent: the sanitised fragment lands in the LIGHT DOM,
// where a kernel-supplied class name would borrow arbitrary dashboard styling.
const HTML_ALLOWED_ATTRS = {
  '*': ['title', 'dir', 'lang'],
  a: ['href'],
  img: ['src', 'alt', 'width', 'height'],
  td: ['colspan', 'rowspan'],
  th: ['colspan', 'rowspan', 'scope'],
  col: ['span'],
  colgroup: ['span'],
  ol: ['start'],
};

const SAFE_HREF = /^(?:https?:|mailto:)/i;
const SAFE_IMAGE_SRC = /^(?:https?:\/\/|data:image\/(?:png|jpeg|jpg|gif|webp);base64,)/i;

/// Inline style is an allowlist like every other decision here. Only properties
/// that decorate content in flow are kept; `position`, `inset`, `z-index`,
/// `transform` and friends are absent because they let kernel output escape its
/// own box and cover the dashboard behind it.
const STYLE_ALLOWED_PROPS = new Set([
  'color', 'background-color', 'opacity',
  'font-family', 'font-size', 'font-style', 'font-weight', 'font-variant',
  'line-height', 'letter-spacing', 'text-align', 'text-decoration',
  'text-transform', 'white-space', 'word-break', 'vertical-align',
  'border', 'border-color', 'border-style', 'border-width', 'border-radius',
  'border-top', 'border-right', 'border-bottom', 'border-left',
  'padding', 'padding-top', 'padding-right', 'padding-bottom', 'padding-left',
  'margin', 'margin-top', 'margin-right', 'margin-bottom', 'margin-left',
  'width', 'height', 'max-width', 'max-height', 'min-width', 'min-height',
]);

// External fetches, legacy script vectors and CSS escapes that would smuggle
// either past the property allowlist.
const UNSAFE_STYLE_VALUE = /url\s*\(|image-set\s*\(|expression\s*\(|@import|javascript:|\\/i;

/// Keeps only the declarations whose property is allowlisted and whose value
/// carries no fetch or escape. Returns '' when nothing survives, so the caller
/// drops the attribute rather than emitting an empty one.
export function sanitizeStyle(style) {
  const kept = [];
  for (const declaration of String(style ?? '').split(';')) {
    const colon = declaration.indexOf(':');
    if (colon < 0) continue;
    const property = declaration.slice(0, colon).trim().toLowerCase();
    const value = declaration.slice(colon + 1).trim();
    if (!value || !STYLE_ALLOWED_PROPS.has(property)) continue;
    if (UNSAFE_STYLE_VALUE.test(value)) continue;
    kept.push(`${property}:${value}`);
  }
  return kept.join(';');
}

export function sanitizeHtml(html) {
  const fragment = document.createDocumentFragment();
  const template = document.createElement('template');
  template.innerHTML = String(html ?? '');
  const source = template.content || template;
  for (const node of Array.from(source.childNodes)) appendSanitized(fragment, node, 0);
  return fragment;
}

function appendSanitized(parent, node, depth) {
  if (depth > 32) return;
  if (node.nodeType === 3) {
    parent.appendChild(document.createTextNode(node.nodeValue));
    return;
  }
  if (node.nodeType !== 1) return;
  const tag = node.tagName.toLowerCase();
  if (HTML_DROPPED_TAGS.has(tag)) return;
  if (!HTML_ALLOWED_TAGS.has(tag)) {
    // Unknown but harmless wrappers are unwrapped so their text survives.
    for (const child of Array.from(node.childNodes)) appendSanitized(parent, child, depth + 1);
    return;
  }
  const clean = document.createElement(tag);
  const allowed = new Set([...HTML_ALLOWED_ATTRS['*'], ...(HTML_ALLOWED_ATTRS[tag] || []), 'style']);
  for (const attribute of Array.from(node.attributes || [])) {
    const name = attribute.name.toLowerCase();
    if (!allowed.has(name) || name.startsWith('on')) continue;
    const value = attribute.value;
    if (name === 'href' && !SAFE_HREF.test(value.trim())) continue;
    if (name === 'src' && !SAFE_IMAGE_SRC.test(value.trim())) continue;
    if (name === 'style') {
      const style = sanitizeStyle(value);
      if (style) clean.setAttribute(name, style);
      continue;
    }
    clean.setAttribute(name, value);
  }
  if (tag === 'a' && clean.hasAttribute('href')) {
    clean.setAttribute('rel', 'noopener noreferrer nofollow');
    clean.setAttribute('target', '_blank');
  }
  for (const child of Array.from(node.childNodes)) appendSanitized(clean, child, depth + 1);
  parent.appendChild(clean);
}

// ---------------------------------------------------------------------------
// Amplitudes and phase
// ---------------------------------------------------------------------------

/// The phase wheel of §13.6 and the mockups: 0 sits at indigo and the hue runs
/// once around the circle over 2π, so opposite phases are opposite colours.
export function phaseColor(phase) {
  const degrees = ((250 + (Number(phase) || 0) * 180 / Math.PI) % 360 + 360) % 360;
  return `hsl(${degrees.toFixed(1)} 80% 66%)`;
}

/// Normalises the two amplitude shapes a state output really carries — the
/// flat interleaved vector of `QuantumSimulator.amplitudes()`, or the sparse
/// `top` list of a keyframe, whose entries are `AmplitudeGroup` objects
/// (`{index, amplitude: [re, im], partners}`; num-complex serialises a complex
/// as a two-element tuple). `partners` is deliberately not turned into rows:
/// it exists so the animation can interpolate the bars the last gate mixed,
/// and every partner large enough to matter is already in `top`.
export function amplitudeRows(value, numQubits) {
  const rows = [];
  const push = (index, re, im) => {
    const magnitude = Math.hypot(re, im);
    if (magnitude < 1e-9) return;
    rows.push({
      index,
      key: index.toString(2).padStart(Math.max(1, numQubits), '0'),
      re, im, magnitude,
      probability: magnitude * magnitude,
      phase: Math.atan2(im, re),
    });
  };
  if (value && value.amplitudes && value.amplitudes.length) {
    const flat = value.amplitudes;
    for (let i = 0; i * 2 + 1 < flat.length; i += 1) push(i, Number(flat[i * 2]), Number(flat[i * 2 + 1]));
  } else if (Array.isArray(value && value.top)) {
    for (const entry of value.top) {
      if (!entry || !Array.isArray(entry.amplitude)) continue;
      push(Number(entry.index), Number(entry.amplitude[0]), Number(entry.amplitude[1]));
    }
  }
  rows.sort((a, b) => b.probability - a.probability || a.index - b.index);
  return rows;
}

/// The non-zero entries of a `-probs+json` payload as labelled bars, biggest
/// first. The label is the basis state the index stands for, which is what a
/// counts histogram of the same circuit puts on its axis.
export function probabilityRows(value) {
  const list = (value && value.probabilities) || [];
  const numQubits = Math.max(1, Number(value && value.numQubits) || 0);
  const rows = [];
  for (let index = 0; index < list.length; index += 1) {
    const probability = Number(list[index]);
    if (!Number.isFinite(probability) || probability <= 0) continue;
    rows.push({ index, key: index.toString(2).padStart(numQubits, '0'), probability });
  }
  rows.sort((a, b) => b.probability - a.probability || a.index - b.index);
  return rows;
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

class TfMimeOutput extends HTMLElement {
  static get observedAttributes() {
    return ['max-lines', 'max-rows'];
  }

  constructor() {
    super();
    this._bundle = null;
    this._preferred = [];
    this._labels = { ...DEFAULT_LABELS };
    this._expanded = false;
    this._jsonExpanded = new Set(['$']);
    this._built = false;
  }

  connectedCallback() {
    if (!this._built) {
      this._built = true;
      this.classList.add('tf-mime');
    }
    this._render();
  }

  attributeChangedCallback() {
    if (this._built) this._render();
  }

  get bundle() { return this._bundle; }

  set bundle(value) {
    this._bundle = value && typeof value === 'object' ? value : null;
    this._expanded = false;
    this._jsonExpanded = new Set(['$']);
    this._render();
  }

  get preferred() { return this._preferred.slice(); }

  set preferred(value) {
    this._preferred = Array.isArray(value) ? value.map(String) : [];
    this._render();
  }

  get labels() { return { ...this._labels }; }

  set labels(value) {
    this._labels = { ...DEFAULT_LABELS, ...(value || {}) };
    this._render();
  }

  /// The mime type currently drawn — the host's hook for a "change view" menu.
  get mime() { return pickMimeType(this._bundle, this._preferred); }

  _maxLines() {
    const value = Number(this.getAttribute('max-lines'));
    return Number.isFinite(value) && value > 0 ? value : DEFAULT_MAX_LINES;
  }

  _maxRows() {
    const value = Number(this.getAttribute('max-rows'));
    return Number.isFinite(value) && value > 0 ? value : DEFAULT_MAX_ROWS;
  }

  _render() {
    if (!this._built) return;
    this.innerHTML = '';
    this.setAttribute('role', 'group');
    // Written once and never observed: re-entering the render from our own
    // attribute write is an infinite loop, not a refresh.
    if (!this.hasAttribute('aria-label')) this.setAttribute('aria-label', this._labels.output);
    const mime = this.mime;
    if (!mime) {
      const empty = document.createElement('div');
      empty.className = 'tf-mime__empty';
      empty.textContent = this._labels.empty;
      this.appendChild(empty);
      return;
    }
    this.dataset.mime = mime;
    const value = this._bundle[mime];
    const body = document.createElement('div');
    body.className = 'tf-mime__body';
    this.appendChild(body);
    switch (mime) {
      case CIRCUIT_MIME: this._renderCircuit(body, value); break;
      case STATE_MIME: this._renderState(body, value); break;
      case COUNTS_MIME: this._renderCounts(body, value); break;
      case PROBS_MIME: this._renderProbabilities(body, value); break;
      case TRACEBACK_MIME: this._renderTraceback(body, value); break;
      case 'image/png':
      case 'image/jpeg': this._renderImage(body, mime, value); break;
      case 'text/html': this._renderHtml(body, value); break;
      case 'text/markdown': this._renderMarkdown(body, value); break;
      case 'application/json': this._renderJson(body, value); break;
      case 'text/plain': this._renderPlain(body, value); break;
      default: {
        const note = document.createElement('div');
        note.className = 'tf-mime__empty';
        note.textContent = this._labels.unsupported;
        body.appendChild(note);
      }
    }
  }

  /// Wraps a long output in a clamp plus the "show all" button. The decision is
  /// made on the SOURCE (lines, rows), never on measured height: a cell can be
  /// rendered while the notebook is still hidden, where every height is zero.
  _collapse(body, overflowing) {
    if (this._expanded || !overflowing) return;
    body.classList.add('tf-mime__body--clamped');
    this._showAllButton();
  }

  _showAllButton() {
    const more = document.createElement('button');
    more.type = 'button';
    more.className = 'tf-btn tf-btn-sm tf-btn-ghost tf-mime__more';
    more.textContent = this._labels.show_all;
    more.addEventListener('click', () => {
      this._expanded = true;
      this.dispatchEvent(new CustomEvent('expand', {
        bubbles: true, composed: true, detail: { mime: this.mime },
      }));
      this._render();
    });
    this.appendChild(more);
  }

  _renderPlain(body, value) {
    const text = String(value ?? '');
    const pre = document.createElement('pre');
    pre.className = 'tf-mime__text';
    pre.textContent = text;
    body.appendChild(pre);
    this._collapse(body, countLines(text) > this._maxLines());
  }

  /// The markdown renderer lives behind an absolute module path, so it is
  /// imported the moment a markdown part actually appears rather than being
  /// pulled into every page that shows an output.
  _renderMarkdown(body, value) {
    const text = String(value ?? '');
    const holder = document.createElement('div');
    holder.className = 'tf-mime__markdown';
    holder.textContent = text;
    body.appendChild(holder);
    this._collapse(body, countLines(text) > this._maxLines());
    import('/js/lib/md-lite.js').then(({ renderMarkdown }) => {
      if (!holder.isConnected) return;
      holder.innerHTML = renderMarkdown(text);
    }).catch(() => {
      // The source is already in the holder as plain text, which is a correct
      // rendering of markdown; a renderer that fails to load must not turn
      // into an unhandled rejection and must not blank the output.
    });
  }

  _renderHtml(body, value) {
    const holder = document.createElement('div');
    holder.className = 'tf-mime__html';
    holder.appendChild(sanitizeHtml(value));
    body.appendChild(holder);
    this._collapse(body, countLines(String(value ?? '')) > this._maxLines());
  }

  _renderImage(body, mime, value) {
    const raw = String(value ?? '').trim();
    const img = document.createElement('img');
    img.className = 'tf-mime__image';
    img.alt = this._labels.output;
    img.src = raw.startsWith('data:') ? raw : `data:${mime};base64,${raw}`;
    body.appendChild(img);
  }

  _renderJson(body, value) {
    const tree = document.createElement('tf-tree');
    tree.nodes = [jsonNode('$', value, '$', 0)];
    tree.expandedIds = Array.from(this._jsonExpanded);
    const toggle = (event, open) => {
      if (open) this._jsonExpanded.add(event.detail.id);
      else this._jsonExpanded.delete(event.detail.id);
      tree.expandedIds = Array.from(this._jsonExpanded);
    };
    tree.addEventListener('expand', (event) => toggle(event, true));
    tree.addEventListener('collapse', (event) => toggle(event, false));
    body.appendChild(tree);
  }

  _renderTraceback(body, value) {
    const text = Array.isArray(value) ? value.join('\n') : String(value ?? '');
    const pre = document.createElement('pre');
    pre.className = 'tf-mime__traceback';
    for (const line of text.split('\n')) {
      const row = document.createElement('span');
      row.className = `tf-mime__tb tf-mime__tb--${tracebackKind(line)}`;
      row.textContent = `${line}\n`;
      pre.appendChild(row);
    }
    body.appendChild(pre);
    this._collapse(body, countLines(text) > this._maxLines());
  }

  _renderCircuit(body, value) {
    const circuit = value && value.circuit ? value.circuit : value;
    const element = document.createElement('tf-quantum-circuit');
    element.setAttribute('readonly', '');
    element.setAttribute('palette', 'none');
    body.appendChild(element);
    element.circuit = circuit;
    if (value && value.step != null) element.step = value.step;
  }

  _renderCounts(body, value) {
    const series = countsSeries(value, this._labels);
    const categories = new Set();
    for (const entry of series) for (const key of Object.keys(entry.counts)) categories.add(key);
    const keys = Array.from(categories).sort();
    const chart = document.createElement('tf-bar-chart');
    chart.height = COUNTS_CHART_HEIGHT;
    chart.xAxis = { scale: 'category' };
    chart.stacking = 'none';
    chart.legend = series.length > 1 ? { position: 'bottom' } : null;
    chart.series = series.map((entry, index) => ({
      id: entry.id,
      name: entry.name,
      tone: entry.tone || (index === 0 ? 'primary' : 'accent'),
      showInLegend: series.length > 1,
      points: keys.map((key) => ({ x: key, y: Number(entry.counts[key]) || 0 })),
    }));
    body.appendChild(chart);
    const shots = Number(value && value.shots);
    if (Number.isFinite(shots) && shots > 0) {
      const foot = document.createElement('div');
      foot.className = 'tf-mime__foot';
      foot.textContent = `${shots} ${this._labels.shots}`;
      body.appendChild(foot);
    }
  }

  /// The exact distribution of a state (`-probs+json`): a dense array of 2^n
  /// numbers indexed by basis state. It is the same picture as a histogram, so
  /// it is the same renderer — the array is turned into the labelled map
  /// `_renderCounts` takes. Only the largest bars are drawn: past a few dozen
  /// columns the chart is a solid block, and "show all" opens the rest the way
  /// every other long output in this element does.
  _renderProbabilities(body, value) {
    const rows = probabilityRows(value);
    if (!rows.length) return;
    const limit = this._expanded ? rows.length : Math.min(rows.length, this._maxRows());
    const counts = {};
    for (const row of rows.slice(0, limit)) counts[row.key] = row.probability;
    this._renderCounts(body, { counts });
    if (rows.length > limit) this._showAllButton();
  }

  _renderState(body, value) {
    const bloch = blochVectorList(value);
    const numQubits = Number(value && value.numQubits) || bloch.length;
    if (bloch.length) {
      const row = document.createElement('div');
      row.className = 'tf-mime__bloch-row';
      bloch.forEach((vector, qubit) => {
        const sphere = document.createElement('tf-bloch-sphere');
        sphere.setAttribute('label', `q${qubit}`);
        sphere.setAttribute('size', '84');
        row.appendChild(sphere);
        sphere.vector = vector;
        const purity = value.purity && value.purity[qubit];
        if (Number.isFinite(Number(purity))) sphere.purity = Number(purity);
      });
      body.appendChild(row);
    }
    const rows = amplitudeRows(value, numQubits);
    if (!rows.length) return;
    const limit = this._expanded ? rows.length : Math.min(rows.length, this._maxRows());
    const table = document.createElement('table');
    table.className = 'tf-mime__amps';
    const head = document.createElement('tr');
    for (const title of [this._labels.basis, this._labels.amplitude, this._labels.probability, this._labels.phase]) {
      const th = document.createElement('th');
      th.textContent = title;
      head.appendChild(th);
    }
    table.appendChild(head);
    for (const entry of rows.slice(0, limit)) {
      table.appendChild(amplitudeRow(entry));
    }
    body.appendChild(table);
    if (rows.length > limit) this._showAllButton();
  }
}

function amplitudeRow(entry) {
  const tr = document.createElement('tr');
  const basis = document.createElement('td');
  basis.textContent = `|${entry.key}⟩`;
  tr.appendChild(basis);

  const amplitude = document.createElement('td');
  amplitude.textContent = `${entry.re.toFixed(3)}${entry.im < 0 ? '−' : '+'}${Math.abs(entry.im).toFixed(3)}i`;
  tr.appendChild(amplitude);

  const probability = document.createElement('td');
  const bar = document.createElement('span');
  bar.className = 'tf-mime__ampbar';
  bar.style.width = `${(entry.probability * 100).toFixed(1)}%`;
  bar.style.background = phaseColor(entry.phase);
  probability.appendChild(bar);
  const number = document.createElement('span');
  number.className = 'tf-mime__ampval';
  number.textContent = entry.probability.toFixed(4);
  probability.appendChild(number);
  tr.appendChild(probability);

  const phase = document.createElement('td');
  const dot = document.createElement('i');
  dot.className = 'tf-mime__phase';
  dot.style.background = phaseColor(entry.phase);
  phase.appendChild(dot);
  phase.appendChild(document.createTextNode(`${(entry.phase / Math.PI).toFixed(2)}π`));
  tr.appendChild(phase);
  return tr;
}

/// Accepts both shapes a counts output takes: one map, or several named series
/// (ideal / noisy simulation / QPU) that the histogram overlays.
function countsSeries(value, labels) {
  if (!value || typeof value !== 'object') return [];
  if (Array.isArray(value.series)) {
    return value.series
      .filter((entry) => entry && entry.counts)
      .map((entry, index) => ({
        id: String(entry.id || entry.name || index),
        name: String(entry.name || entry.id || index),
        tone: entry.tone,
        counts: entry.counts,
      }));
  }
  if (value.counts && typeof value.counts === 'object') {
    return [{ id: 'counts', name: labels.ideal, counts: value.counts }];
  }
  return [];
}

function jsonNode(key, value, path, depth) {
  if (depth > 24 || value === null || typeof value !== 'object') {
    return { id: path, label: `${key}: ${formatScalar(value)}` };
  }
  const entries = Array.isArray(value)
    ? value.map((item, index) => [String(index), item])
    : Object.entries(value);
  const summary = Array.isArray(value) ? `[${entries.length}]` : `{${entries.length}}`;
  return {
    id: path,
    label: `${key} ${summary}`,
    children: entries.map(([childKey, child]) => jsonNode(childKey, child, `${path}.${childKey}`, depth + 1)),
  };
}

function formatScalar(value) {
  if (typeof value === 'string') return JSON.stringify(value);
  return String(value);
}

function tracebackKind(line) {
  if (/^\s*File "/.test(line)) return 'file';
  if (/^\s*\^+\s*$/.test(line)) return 'caret';
  if (/^Traceback/.test(line)) return 'head';
  if (/^[A-Za-z_][\w.]*(Error|Exception|Warning|Interrupt)\b/.test(line)) return 'error';
  return 'plain';
}

function countLines(text) {
  return String(text ?? '').split('\n').length;
}

if (!customElements.get('tf-mime-output')) {
  customElements.define('tf-mime-output', TfMimeOutput);
}

export { TfMimeOutput, DEFAULT_LABELS as MIME_LABELS };
