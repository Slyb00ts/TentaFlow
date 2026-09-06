// =============================================================================
// File: components/tf-entanglement-graph.js
// Description: <tf-entanglement-graph> — the entanglement map of plan §13.6
//              (mockup Q15): the qubits ON THE CIRCUIT'S OWN ROWS, and an arc
//              between every pair that shares something. Edge THICKNESS is the
//              mutual information (how much the two qubits know about each
//              other, in bits), edge COLOUR is the concurrence (how much of
//              that is genuinely quantum).
//
//              The two numbers are deliberately not merged into one: a pair of
//              classically correlated qubits carries mutual information with
//              zero concurrence, and a picture that showed one number could not
//              tell that apart from a Bell pair. Both come straight off a
//              `KeyframePair`, so the component computes no physics of its own.
//
//              Rows, not a force layout: the qubits keep the vertical order
//              they have in the circuit strip and the Bloch row above, so the
//              eye carries q2 from one picture to the next.
//
//  Properties: pairs — [{qubits: [i, j], mutualInformation, concurrence}],
//              numQubits, labels — i18n dict.
//  Attributes: height (px per qubit row, default 34), width.
//
// Example: graph.numQubits = 4; graph.pairs = frame.pairs;
// =============================================================================

const SVG_NS = 'http://www.w3.org/2000/svg';
const DEFAULT_ROW = 34;
const DEFAULT_WIDTH = 420;

/// Mutual information of a qubit PAIR is bounded by 2 bits (both marginals are
/// one qubit), which is what the thickness scale is normalised against.
export const MAX_MUTUAL_INFORMATION = 2;

const DEFAULT_LABELS = {
  entanglement: 'Entanglement map',
  empty: 'no correlated pairs',
  mutual: 'mutual information',
  concurrence: 'concurrence',
  bits: 'bit',
};

// ---------------------------------------------------------------------------
// Layout — pure
// ---------------------------------------------------------------------------

/// One node per qubit, stacked in circuit order. `x` is the wire's left anchor;
/// every arc bulges to the right of it, which is where the space is.
export function graphLayout(numQubits, { width = DEFAULT_WIDTH, row = DEFAULT_ROW } = {}) {
  const n = Math.max(0, Number(numQubits) || 0);
  const rowHeight = Math.max(18, Number(row) || DEFAULT_ROW);
  return {
    width: Math.max(120, Number(width) || DEFAULT_WIDTH),
    height: Math.max(rowHeight, n * rowHeight),
    nodes: Array.from({ length: n }, (_, qubit) => ({
      qubit,
      x: 44,
      y: rowHeight * qubit + rowHeight / 2,
    })),
  };
}

/// Thickness of one edge in pixels. A pair with a whisper of correlation still
/// gets a visible hairline: "thin" and "absent" must not look the same.
export function edgeWidth(mutualInformation) {
  const mi = Math.max(0, Math.min(MAX_MUTUAL_INFORMATION, Number(mutualInformation) || 0));
  return 1 + (mi / MAX_MUTUAL_INFORMATION) * 7;
}

/// Colour of one edge by concurrence: 0 (classical correlation only) is the
/// muted grey of a wire, 1 (a maximally entangled pair) the accent pink every
/// other view of this app uses for "entangled".
export function concurrenceColor(concurrence) {
  const c = Math.max(0, Math.min(1, Number(concurrence) || 0));
  const hue = 250 - c * 30;
  const saturation = 12 + c * 68;
  const lightness = 62 + c * 8;
  return `hsl(${hue.toFixed(1)} ${saturation.toFixed(1)}% ${lightness.toFixed(1)}%)`;
}

/// The pairs worth drawing, strongest first: an arc for a pair whose mutual
/// information rounds to nothing is ink that says nothing.
export function visibleEdges(pairs, threshold = 1e-4) {
  return (pairs || [])
    .map((pair) => {
      const qubits = Array.from(pair.qubits || [], Number);
      return {
        a: Math.min(qubits[0], qubits[1]),
        b: Math.max(qubits[0], qubits[1]),
        mutualInformation: Number(pair.mutualInformation ?? pair.mutual_information) || 0,
        concurrence: Number(pair.concurrence) || 0,
      };
    })
    .filter((edge) => Number.isInteger(edge.a) && Number.isInteger(edge.b)
      && edge.a !== edge.b && edge.mutualInformation > threshold)
    .sort((x, y) => y.mutualInformation - x.mutualInformation);
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

class TfEntanglementGraph extends HTMLElement {
  static get observedAttributes() {
    return ['height', 'width'];
  }

  constructor() {
    super();
    this._pairs = [];
    this._numQubits = 0;
    this._labels = { ...DEFAULT_LABELS };
    this._built = false;
  }

  connectedCallback() {
    if (!this._built) this._build();
    this._render();
  }

  attributeChangedCallback() {
    if (this._built) this._render();
  }

  get pairs() { return this._pairs; }

  set pairs(value) {
    this._pairs = Array.isArray(value) ? value : [];
    if (!this._built) this._build();
    this._render();
  }

  get numQubits() { return this._numQubits; }

  set numQubits(value) {
    this._numQubits = Math.max(0, Number(value) || 0);
    if (!this._built) this._build();
    this._render();
  }

  get labels() { return { ...this._labels }; }

  set labels(value) {
    this._labels = { ...DEFAULT_LABELS, ...(value || {}) };
    if (this._built) this._render();
  }

  _build() {
    this._built = true;
    this.classList.add('tf-entgraph');
    this._svg = document.createElementNS(SVG_NS, 'svg');
    this._svg.setAttribute('class', 'tf-entgraph__svg');
    this._svg.setAttribute('role', 'img');
    this._note = document.createElement('div');
    this._note.className = 'tf-entgraph__note';
    this.replaceChildren(this._svg, this._note);
  }

  _render() {
    const rowHeight = Number(this.getAttribute('height')) || DEFAULT_ROW;
    const width = Number(this.getAttribute('width')) || DEFAULT_WIDTH;
    const edges = visibleEdges(this._pairs);
    const qubits = Math.max(this._numQubits, edges.reduce((top, e) => Math.max(top, e.b + 1), 0));
    const layout = graphLayout(qubits, { width, row: rowHeight });
    this._svg.setAttribute('viewBox', `0 0 ${layout.width} ${layout.height}`);
    this._svg.setAttribute('width', String(layout.width));
    this._svg.setAttribute('height', String(layout.height));
    this._svg.replaceChildren();
    for (const node of layout.nodes) {
      const wire = document.createElementNS(SVG_NS, 'line');
      wire.setAttribute('class', 'tf-entgraph__wire');
      wire.setAttribute('x1', String(node.x));
      wire.setAttribute('y1', node.y.toFixed(2));
      wire.setAttribute('x2', String(layout.width - 8));
      wire.setAttribute('y2', node.y.toFixed(2));
      this._svg.appendChild(wire);
      const label = document.createElementNS(SVG_NS, 'text');
      label.setAttribute('class', 'tf-entgraph__label');
      label.setAttribute('x', String(node.x - 10));
      label.setAttribute('y', (node.y + 4).toFixed(2));
      label.setAttribute('text-anchor', 'end');
      label.textContent = `q${node.qubit}`;
      this._svg.appendChild(label);
    }
    // Thin arcs first: a strong pair must never be buried under a faint one.
    for (const edge of edges.slice().reverse()) {
      const from = layout.nodes[edge.a];
      const to = layout.nodes[edge.b];
      if (!from || !to) continue;
      const span = Math.abs(to.y - from.y);
      const bulge = Math.min(layout.width * 0.42, 40 + span * 0.55);
      const arc = document.createElementNS(SVG_NS, 'path');
      arc.setAttribute('class', 'tf-entgraph__edge');
      arc.setAttribute('d', `M${from.x} ${from.y.toFixed(2)} Q${(from.x + bulge).toFixed(2)} ${((from.y + to.y) / 2).toFixed(2)} ${to.x} ${to.y.toFixed(2)}`);
      arc.setAttribute('stroke', concurrenceColor(edge.concurrence));
      arc.setAttribute('stroke-width', edgeWidth(edge.mutualInformation).toFixed(2));
      const title = document.createElementNS(SVG_NS, 'title');
      title.textContent = `q${edge.a} · q${edge.b} — ${this._labels.mutual} ${edge.mutualInformation.toFixed(3)} ${this._labels.bits}`
        + ` · ${this._labels.concurrence} ${edge.concurrence.toFixed(3)}`;
      arc.appendChild(title);
      this._svg.appendChild(arc);
    }
    for (const node of layout.nodes) {
      const dot = document.createElementNS(SVG_NS, 'circle');
      dot.setAttribute('class', 'tf-entgraph__node');
      dot.setAttribute('cx', String(node.x));
      dot.setAttribute('cy', node.y.toFixed(2));
      dot.setAttribute('r', '4');
      this._svg.appendChild(dot);
    }
    this._note.textContent = edges.length ? '' : this._labels.empty;
    this._svg.setAttribute('aria-label', edges.length
      ? `${this._labels.entanglement}: ${edges.map((e) => `q${e.a}–q${e.b} ${e.mutualInformation.toFixed(2)}`).join(', ')}`
      : `${this._labels.entanglement}: ${this._labels.empty}`);
  }
}

if (!customElements.get('tf-entanglement-graph')) {
  customElements.define('tf-entanglement-graph', TfEntanglementGraph);
}

export { TfEntanglementGraph, DEFAULT_LABELS as ENTANGLEMENT_LABELS };
