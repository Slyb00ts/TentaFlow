// =============================================================================
// Plik: sdk-runtime/domain-relation-graph-renderer.test.js
// Opis: Testy renderera RelationGraph (0x0703) — mapowanie FieldMap na
// <tf-relation-graph>, sanityzacja danych grafu ze store, reaktywne
// subskrypcje, eventy node_click/edge_click i walidacja pól.
// =============================================================================

import './_dom-test-harness.js';

import '../components/tf-relation-graph.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import {
  registerDomainRelationGraphRenderer,
  RELATION_GRAPH_TAG,
  readGraphNodes,
  readGraphEdges,
} from './domain-relation-graph-renderer.js';

const results = [];
function test(name, fn) {
  try {
    fn();
    results.push({ name, ok: true });
  } catch (err) {
    results.push({ name, ok: false, err });
  }
}
function assertEq(actual, expected, msg) {
  const a = JSON.stringify(actual, (_k, v) => (typeof v === 'bigint' ? `${v}n` : v));
  const b = JSON.stringify(expected, (_k, v) => (typeof v === 'bigint' ? `${v}n` : v));
  if (a !== b) throw new Error(`${msg || 'assertEq'}: expected ${b}, got ${a}`);
}
function assert(cond, msg) {
  if (!cond) throw new Error(msg || 'assert failed');
}
function assertThrows(fn, msg) {
  let threw = false;
  try { fn(); } catch { threw = true; }
  if (!threw) throw new Error(msg || 'expected throw');
}

const NODES_PATH = [{ kind: 'key', value: 'graph' }, { kind: 'key', value: 'nodes' }];
const EDGES_PATH = [{ kind: 'key', value: 'graph' }, { kind: 'key', value: 'edges' }];

function makeStore(nodes, edges) {
  const store = new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
  store.applySnapshot({
    entries: [
      { path: NODES_PATH, value: nodes },
      { path: EDGES_PATH, value: edges },
    ],
    state_revision: 0,
    truncated: false,
  });
  return store;
}

function graphFields(overrides = {}) {
  const base = {
    0: NODES_PATH,
    1: EDGES_PATH,
    2: 'force_directed',
    3: true,
    4: 500,
  };
  Object.assign(base, overrides);
  return Object.entries(base)
    .filter(([, v]) => v !== undefined)
    .map(([k, v]) => [Number(k), v]);
}

function comp(fields, extra = {}) {
  return {
    tag: RELATION_GRAPH_TAG,
    id: 'graph1',
    fields,
    handlers: extra.handlers ?? null,
    bind: null,
    a11y: null,
    visibility: null,
    test_id: null,
  };
}

const SAMPLE_NODES = [
  { id: 'n1', label: 'Meeting note', node_type: 'note', tone: 'primary' },
  { id: 'n2', label: 'Anna Kowalska', node_type: 'person', tone: 'info' },
  { id: 'n3', label: 'Firma Sp. z o.o.', node_type: 'company', tone: 'success' },
];
const SAMPLE_EDGES = [
  { id: 'e1', source_id: 'n1', target_id: 'n2', weight: 1.5 },
  { id: 'e2', source_id: 'n1', target_id: 'n3', style: 'dashed', label: '87%' },
];

function setup(nodes = SAMPLE_NODES, edges = SAMPLE_EDGES) {
  _clearComponentRendererRegistry();
  registerDomainRelationGraphRenderer();
  document.body.innerHTML = '';
  const store = makeStore(nodes, edges);
  const emitted = [];
  const engine = new ComponentRenderer({
    store,
    eventDispatcher: { emit(ev) { emitted.push(ev); } },
    locale: 'en-US',
  });
  return { store, engine, emitted };
}

test('RelationGraph renders tf-relation-graph with mapped props', () => {
  const { engine } = setup();
  const el = engine.render(comp(graphFields()));
  document.body.appendChild(el);
  assertEq(el.tagName.toLowerCase(), 'tf-relation-graph');
  assertEq(el.layout, 'force_directed');
  assertEq(el.interactive, true);
  assertEq(el.maxNodes, 500);
  assertEq(el.nodes.length, 3);
  assertEq(el.nodes[0], {
    id: 'n1', label: 'Meeting note', nodeType: 'note', tone: 'primary',
    icon: null, x: null, y: null,
  });
  assertEq(el.edges.length, 2);
  assertEq(el.edges[1], {
    id: 'e2', sourceId: 'n1', targetId: 'n3', label: '87%',
    weight: null, tone: null, dashed: true,
  });
});

test('RelationGraph store update re-pushes nodes reactively', () => {
  const { store, engine } = setup();
  const el = engine.render(comp(graphFields()));
  document.body.appendChild(el);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: NODES_PATH, op: { kind: 'set', value: [SAMPLE_NODES[0]] } }],
  });
  assertEq(el.nodes.length, 1);
});

test('RelationGraph destroy unsubscribes from the store', () => {
  const { store, engine } = setup();
  const el = engine.render(comp(graphFields()));
  document.body.appendChild(el);
  engine.destroy(el);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: NODES_PATH, op: { kind: 'set', value: [] } }],
  });
  // Subscription released — the component keeps the last pushed data.
  assertEq(el.nodes.length, 3);
});

test('readGraphNodes skips malformed and duplicate entries', () => {
  const { store } = setup(
    [
      { id: 'ok', label: 'Ok', node_type: 't' },
      { label: 'no id' },
      null,
      'not-an-object',
      { id: 'ok', label: 'duplicate' },
      { id: 'weird', label: 42, tone: 'not-a-tone' },
    ],
    []
  );
  const nodes = readGraphNodes(store, NODES_PATH);
  assertEq(nodes.map((n) => n.id), ['ok', 'weird']);
  assertEq(nodes[1].label, '');
  assertEq(nodes[1].tone, null);
});

test('readGraphEdges skips entries without endpoints', () => {
  const { store } = setup([], [
    { id: 'e1', source_id: 'a', target_id: 'b', weight: 2 },
    { id: 'e2', source_id: 'a' },
    { source_id: 'a', target_id: 'b' },
  ]);
  const edges = readGraphEdges(store, EDGES_PATH);
  assertEq(edges.map((e) => e.id), ['e1']);
  assertEq(edges[0].weight, 2);
  assertEq(edges[0].dashed, false);
});

test('component drops edges referencing missing nodes and caps max_nodes', () => {
  const { engine } = setup(
    [
      { id: 'n1', label: 'A', node_type: 't' },
      { id: 'n2', label: 'B', node_type: 't' },
      { id: 'n3', label: 'C', node_type: 't' },
    ],
    [
      { id: 'e1', source_id: 'n1', target_id: 'n2' },
      { id: 'e2', source_id: 'n2', target_id: 'ghost' },
      { id: 'e3', source_id: 'n1', target_id: 'n3' },
    ]
  );
  const el = engine.render(comp(graphFields({ 4: 2 })));
  document.body.appendChild(el);
  // max_nodes=2 keeps n1/n2; e2 (ghost) and e3 (n3 dropped) disappear.
  assertEq(el._sim.size, 2);
  assertEq(el._edgeList.map((e) => e.id), ['e1']);
});

test('node_click handler receives node_id via CustomEvent detail', () => {
  const { engine, emitted } = setup();
  const handler = { kind: 'backend', action_id: 'open_node', params: {} };
  const el = engine.render(comp(graphFields(), { handlers: [['node_click', handler]] }));
  document.body.appendChild(el);
  el.dispatchEvent(new CustomEvent('node_click', { detail: { node_id: 'n2' } }));
  assertEq(emitted.length, 1);
  assertEq(emitted[0].event_kind, 'node_click');
  assertEq(emitted[0].source_id, 'graph1');
  assertEq(emitted[0].dom_event.detail, { node_id: 'n2' });
});

test('edge_click handler receives edge_id via CustomEvent detail', () => {
  const { engine, emitted } = setup();
  const handler = { kind: 'backend', action_id: 'open_edge', params: {} };
  const el = engine.render(comp(graphFields(), { handlers: [['edge_click', handler]] }));
  document.body.appendChild(el);
  el.dispatchEvent(new CustomEvent('edge_click', { detail: { edge_id: 'e2' } }));
  assertEq(emitted.length, 1);
  assertEq(emitted[0].event_kind, 'edge_click');
  assertEq(emitted[0].dom_event.detail, { edge_id: 'e2' });
});

test('RelationGraph validation rejects malformed fields', () => {
  const { engine } = setup();
  const cases = [
    graphFields({ 0: undefined }),               // missing nodes_path
    graphFields({ 1: undefined }),               // missing edges_path
    graphFields({ 0: 'not-a-path' }),            // nodes_path not an array
    graphFields({ 2: 'circular' }),              // unknown GraphLayout
    graphFields({ 3: 'yes' }),                   // interactive not bool
    graphFields({ 4: -1 }),                      // max_nodes not u32
    graphFields().concat([[5, 'x']]),            // unknown field key
  ];
  for (const fields of cases) {
    assertThrows(() => engine.render(comp(fields)));
  }
});

test('selecting via click dispatches node_click from canvas hit test', () => {
  const { engine } = setup();
  const el = engine.render(comp(graphFields({ 2: 'manual' })));
  document.body.appendChild(el);
  // Manual layout: pin every node at a known world position.
  el.nodes = [
    { id: 'n1', label: 'A', nodeType: 't', tone: null, icon: null, x: 0, y: 0 },
    { id: 'n2', label: 'B', nodeType: 't', tone: null, icon: null, x: 200, y: 0 },
  ];
  el.edges = [];
  const clicks = [];
  el.addEventListener('node_click', (e) => clicks.push(e.detail));
  // Zero-slop pointer down/up on the projected position of n1.
  const view = el._view;
  const px = 0 * view.k + view.x;
  const py = 0 * view.k + view.y;
  const cv = el._canvas;
  cv.dispatchEvent(new MouseEvent('pointerdown', { clientX: px, clientY: py, bubbles: true }));
  cv.dispatchEvent(new MouseEvent('pointerup', { clientX: px, clientY: py, bubbles: true }));
  assertEq(clicks, [{ node_id: 'n1' }]);
  assertEq(el.selectedNodeId, 'n1');
});

// Detach every rendered graph so disconnectedCallback cancels pending rAF
// timers — otherwise the happy-dom timer queue keeps the Node process alive.
document.body.innerHTML = '';

function reportResults() {
  let pass = 0;
  let fail = 0;
  const lines = [];
  for (const r of results) {
    if (r.ok) {
      pass += 1;
      lines.push(`✓ ${r.name}`);
    } else {
      fail += 1;
      lines.push(`✗ ${r.name}\n    ${r.err && r.err.stack ? r.err.stack : r.err}`);
    }
  }
  lines.push('');
  lines.push(`${pass}/${pass + fail} tests passed${fail ? ` — ${fail} FAILED` : ''}`);
  return { pass, fail, text: lines.join('\n') };
}

if (typeof process !== 'undefined') {
  const r = reportResults();
  // eslint-disable-next-line no-console
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}

export { reportResults };
