// =============================================================================
// File: modules/code-studio.picker-note.test.js
// Description: Unit tests for the two isolation cards of the Code Studio
//       creation wizard. The node picker used to print the RUST probe's English
//       sentence verbatim, so a Polish dashboard read "process isolation requires
//       the current user's GUI launchd domain" — an untranslated internal string
//       standing in for a message. The owner node now reports a CAUSE on
//       `WorkspaceNodeInfo` and the picker chooses a localized sentence, with the
//       raw text demoted to the tooltip. The container card carries the same
//       kind of fact about the SAME row, and it is a tri-state for the same
//       reason: a peer that reported nothing must not read as a peer that has no
//       runtime. These tests drive the REAL `renderModes`, `processSandboxNote`
//       and `containerNote` cut out of the shipped module (the module pulls the
//       whole dashboard in, so it cannot be imported), one row per cause.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'code-studio.js'), 'utf8');
const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];

// Index just past the closing brace of the block that opens at/after `from`.
function braceEnd(src, from) {
  let depth = 0;
  for (let i = src.indexOf('{', from); i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
  }
  throw new Error('unbalanced braces');
}

// `processSandboxNote` is a function declaration, `renderModes` an arrow const;
// both are cut out of the real file by brace matching, so the code under test is
// the shipped code and not a copy that can drift away from it.
function cut(src, name) {
  const decl = src.indexOf(`function ${name}(`);
  if (decl >= 0) return src.slice(decl, braceEnd(src, decl));
  const binding = src.indexOf(`const ${name} = `);
  if (binding < 0) throw new Error(`no definition: ${name}`);
  return `${src.slice(binding, braceEnd(src, binding))};`;
}

const PRELUDE = `
  const state = { nodes: [] };
  const byId = (id) => elements[id];
  const tCalls = [];
  const t = (key, params) => { tCalls.push([key, params]); return key; };
  const wz = { nodeId: '', execMode: 'process_sandbox', repoKind: 'empty' };
  const elements = {
    'cs-wz-sandbox-repair': {
      hidden: true,
      message: '',
      setAttribute(name, value) { if (name === 'message') this.message = value; },
    },
  };
`;

// The repair helpers the module imports from lib/sandbox-repair.js. That
// module loads i18n, which fetches a locale on import, so these tests hand in
// stand-ins with the same contract: only `user_namespaces_denied` repairs.
const sandboxRepairable = (cause) => cause === 'user_namespaces_denied';
const sandboxRepairProblem = (node) => `sandbox_repair.problem_userns:${node}`;

function build(names) {
  const body = names.map((n) => cut(source, n)).join('\n');
  // eslint-disable-next-line no-new-func
  return new Function(
    'sandboxRepairable',
    'sandboxRepairProblem',
    `${PRELUDE}\n${body}\nreturn { ${names.join(', ')}, elements, state, wz, tCalls };`,
  )(sandboxRepairable, sandboxRepairProblem);
}

// One fake choice group per test: the three cards the wizard renders, plus the
// `value` assignment `renderModes` finishes with. Both isolation modes sit on
// the SAME fake row, because a node's two advertisements must be readable
// together — that is how the picker paints them.
function paintCards(node, wzOverrides = {}, onApi = null) {
  const api = build(['nodeById', 'processSandboxNote', 'containerNote', 'containerFactKey', 'renderModes']);
  const card = () => ({ disabled: false, note: '', title: '' });
  const group = {
    value: '',
    cards: { container: card(), process_sandbox: card(), trusted_native: card() },
    querySelector(sel) {
      if (sel.includes('container')) return this.cards.container;
      if (sel.includes('process_sandbox')) return this.cards.process_sandbox;
      return this.cards.trusted_native;
    },
  };
  api.elements['cs-wz-modes'] = group;
  api.state.nodes = [node];
  api.wz.nodeId = String(node.nodeId ?? node.node_id);
  Object.assign(api.wz, wzOverrides);
  api.renderModes();
  onApi?.(api);
  return group.cards;
}

function paint(node, wzOverrides) {
  return paintCards(node, wzOverrides).process_sandbox;
}

const RAW = "process isolation requires the current user's GUI launchd domain";

// ---------------------------------------------------------------------------
// processSandboxNote — the cause picks the sentence
// ---------------------------------------------------------------------------

test('processSandboxNote: the GUI-session cause gets its own explicit sentence', () => {
  const api = build(['processSandboxNote']);
  assert.equal(api.processSandboxNote({
    processSandboxCause: 'gui_session_required', isLocal: true, processSandboxReason: RAW,
  }), 'mode_process_gui_session');
});

test('processSandboxNote: every other cause of a local node shares the generic key', () => {
  const api = build(['processSandboxNote']);
  const others = ['no_sandbox_binary', 'supervisor_not_initialized', 'missing_coalition'];
  for (const cause of others) {
    assert.equal(api.processSandboxNote({ processSandboxCause: cause, isLocal: true }), 'mode_process_unavailable');
  }
});

test('processSandboxNote: a remote node that reported NO cause is told its OWNER must answer', () => {
  const api = build(['processSandboxNote']);
  assert.equal(api.processSandboxNote({ isLocal: false }), 'mode_process_remote');
  // The old payload spelled the same absence as an English sentence; the
  // sentence is gone and the answer must not change.
  assert.equal(api.processSandboxNote({
    isLocal: false,
    processSandboxReason: 'Availability must be checked on the owner node',
  }), 'mode_process_remote');
});

test('processSandboxNote: a remote refusal uses ITS cause, not "ask the owner"', () => {
  const api = build(['processSandboxNote']);
  assert.equal(api.processSandboxNote({
    is_local: false, supports_process_sandbox: false, process_sandbox_cause: 'gui_session_required',
  }), 'mode_process_gui_session');
  assert.equal(api.processSandboxNote({
    is_local: false, supports_process_sandbox: false, process_sandbox_cause: 'no_sandbox_binary',
  }), 'mode_process_unavailable');
});

test('processSandboxNote: an unknown cause never falls back to the raw English reason', () => {
  const api = build(['processSandboxNote']);
  const note = api.processSandboxNote({
    processSandboxCause: 'some_future_cause', isLocal: true, processSandboxReason: RAW,
  });
  assert.equal(note, 'mode_process_unavailable');
  assert.doesNotMatch(note, /launchd/);
});

test('processSandboxNote: a cause is read off snake_case payloads too', () => {
  const api = build(['processSandboxNote']);
  assert.equal(api.processSandboxNote({ process_sandbox_cause: 'gui_session_required', is_local: true }),
    'mode_process_gui_session');
});

// ---------------------------------------------------------------------------
// renderModes — the card carries the sentence, the tooltip carries the probe
// ---------------------------------------------------------------------------

test('renderModes: a local node without a GUI session blocks the card in the reader language', () => {
  const card = paint({
    nodeId: 'mainpc',
    isLocal: true,
    supportsContainer: true,
    supportsProcessSandbox: false,
    processSandboxCause: 'gui_session_required',
    processSandboxReason: RAW,
  });
  assert.equal(card.disabled, true);
  assert.equal(card.note, 'mode_process_gui_session');
  assert.equal(card.title, RAW, 'the probe sentence stays reachable for diagnosis');
});

// A kernel that denies bwrap its namespaces is the one cause the node repairs
// itself, so the blocked card gets the repair right under it; a cause no
// password fixes gets no button.
test('renderModes: a repairable cause offers the repair, another cause does not', () => {
  let alert = null;
  paintCards({
    nodeId: 'spark',
    name: 'spark-002',
    isLocal: true,
    supportsProcessSandbox: false,
    processSandboxCause: 'user_namespaces_denied',
  }, {}, (api) => { alert = api.elements['cs-wz-sandbox-repair']; });
  assert.equal(alert.hidden, false);
  assert.equal(alert.message, 'sandbox_repair.problem_userns:spark-002', 'the alert names the node');

  paintCards({
    nodeId: 'mac',
    isLocal: true,
    supportsProcessSandbox: false,
    processSandboxCause: 'gui_session_required',
  }, {}, (api) => { alert = api.elements['cs-wz-sandbox-repair']; });
  assert.equal(alert.hidden, true);

  paintCards({
    nodeId: 'ok',
    isLocal: true,
    supportsProcessSandbox: true,
  }, {}, (api) => { alert = api.elements['cs-wz-sandbox-repair']; });
  assert.equal(alert.hidden, true, 'a working sandbox has nothing to repair');
});

test('renderModes: a missing sandbox binary blocks the card with the generic sentence', () => {
  const card = paint({
    nodeId: 'mainpc',
    isLocal: true,
    supportsProcessSandbox: false,
    processSandboxCause: 'no_sandbox_binary',
    processSandboxReason: 'process sandbox unavailable: requires macOS sandbox-exec or Linux /usr/bin/bwrap',
  });
  assert.equal(card.note, 'mode_process_unavailable');
  assert.match(card.title, /bwrap/);
});

test('renderModes: a remote node that reported nothing is blocked as "the owner must answer"', () => {
  const card = paint({
    node_id: 'gpu-01',
    is_local: false,
    supports_process_sandbox: null,
    process_sandbox_cause: null,
  });
  assert.equal(card.disabled, true);
  assert.equal(card.note, 'mode_process_remote');
  assert.doesNotMatch(card.note, /owner node/);
  assert.equal(card.title, '');
});

// A peer whose own probe says it CAN isolate a process is selectable — the
// defect this test pins is that every remote row used to arrive without a
// value and was therefore read as a refusal.
test('renderModes: a remote node that advertises support is selectable', () => {
  const card = paint({
    node_id: 'gpu-01',
    is_local: false,
    supports_process_sandbox: true,
  });
  assert.equal(card.disabled, false);
  assert.equal(card.note, '');
  assert.equal(card.title, '');
});

// The cause is the whole point of carrying one: "start it from a desktop
// session" and "this machine will never do it" must not read alike.
test('renderModes: a remote refusal is shown by ITS cause, and two causes differ', () => {
  const gui = paint({
    node_id: 'gpu-01',
    is_local: false,
    supports_process_sandbox: false,
    process_sandbox_cause: 'gui_session_required',
  });
  assert.equal(gui.disabled, true);
  assert.equal(gui.note, 'mode_process_gui_session');

  const binary = paint({
    node_id: 'gpu-01',
    is_local: false,
    supports_process_sandbox: false,
    process_sandbox_cause: 'no_sandbox_binary',
  });
  assert.equal(binary.disabled, true);
  assert.equal(binary.note, 'mode_process_unavailable');
  assert.notEqual(gui.note, binary.note, 'two causes collapsed into one message');
});

test('renderModes: an available sandbox leaves the card enabled and silent', () => {
  const card = paint({
    nodeId: 'mainpc',
    isLocal: true,
    supportsProcessSandbox: true,
    egressEnforcement: 'namespace',
  });
  assert.equal(card.disabled, false);
  assert.equal(card.note, '');
  assert.equal(card.title, '');
});

// ---------------------------------------------------------------------------
// The container card — the same tri-state, on the same row
// ---------------------------------------------------------------------------

test('containerFactKey: the node list says which of the three states it is in', () => {
  const api = build(['containerFactKey']);
  assert.equal(api.containerFactKey(true), 'node_container_yes');
  assert.equal(api.containerFactKey(false), 'node_container_no');
  assert.equal(api.containerFactKey(null), 'node_container_unknown');
  assert.equal(api.containerFactKey(undefined), 'node_container_unknown');
});

test('containerNote: only a node that reported NO runtime is told to install one', () => {
  const api = build(['containerNote']);
  assert.equal(api.containerNote({ name: 'gpu-01' }, true), '', 'a working runtime needs no note');
  // The `false` sentence names the node; the `null` one does not, and the two
  // must not collapse — "install Docker" is a claim about a node that spoke,
  // and a node that stayed silent never made it.
  assert.equal(api.containerNote({ name: 'gpu-01' }, false), 'mode_container_unavailable');
  assert.deepEqual(api.tCalls.at(-1), ['mode_container_unavailable', { node: 'gpu-01' }]);
  assert.equal(api.containerNote({ name: 'gpu-01' }, null), 'mode_container_remote');
  assert.deepEqual(api.tCalls.at(-1), ['mode_container_remote', undefined],
    'the silence sentence must not name a node it never heard from');
});

test('renderModes: a peer advertising a container runtime is offered the mode', () => {
  const { container } = paintCards({ node_id: 'gpu-01', is_local: false, supports_container: true });
  assert.equal(container.disabled, false);
  assert.equal(container.note, '');
});

test('renderModes: a peer advertising NO runtime is refused in the node-owner words', () => {
  const { container } = paintCards({ node_id: 'gpu-01', is_local: false, supports_container: false });
  assert.equal(container.disabled, true);
  assert.equal(container.note, 'mode_container_unavailable');
});

test('renderModes: a peer that advertised nothing is refused as "the owner must confirm"', () => {
  const { container } = paintCards({ node_id: 'gpu-01', is_local: false, supports_container: null });
  assert.equal(container.disabled, true);
  assert.equal(container.note, 'mode_container_remote');
  assert.notEqual(container.note, 'mode_container_unavailable',
    'a silent peer was told to install a runtime it never reported missing');
});

test('renderModes: both isolation modes are read off the one peer row, independently', () => {
  const { container, process_sandbox: process } = paintCards({
    node_id: 'gpu-01',
    is_local: false,
    supports_container: true,
    supports_process_sandbox: false,
    process_sandbox_cause: 'gui_session_required',
  });
  assert.equal(container.disabled, false, 'the container answer must not be dragged down by the sandbox one');
  assert.equal(container.note, '');
  assert.equal(process.disabled, true);
  assert.equal(process.note, 'mode_process_gui_session');
});

test('renderModes: a local repository kind keeps the container mode out of reach', () => {
  const { container } = paintCards(
    { nodeId: 'mainpc', isLocal: true, supportsContainer: true },
    { repoKind: 'local' },
  );
  assert.equal(container.disabled, true);
  assert.equal(container.note, '', 'the repository kind is not the node\'s runtime report');
});

// ---------------------------------------------------------------------------
// The keys the picker asks for must exist everywhere the dashboard speaks
// ---------------------------------------------------------------------------

test('every locale translates the keys both isolation cards can ask for', () => {
  const bundles = LOCALES.map((l) => [l, JSON.parse(
    readFileSync(join(here, '..', '..', 'i18n', `${l}.json`), 'utf8'),
  ).code_studio]);
  const keys = [
    'mode_process_gui_session', 'mode_process_remote', 'mode_process_unavailable',
    'mode_container_unavailable', 'mode_container_remote',
    'node_container_yes', 'node_container_no', 'node_container_unknown',
    'enforcement_namespace', 'enforcement_unrestricted',
  ];
  for (const [locale, cs] of bundles) {
    for (const key of keys) {
      assert.equal(typeof cs[key], 'string', `${locale}.json is missing code_studio.${key}`);
      assert.ok(cs[key].trim().length > 0, `${locale}.json has an empty code_studio.${key}`);
    }
  }
});

test('the three container facts are three different messages in every locale', () => {
  for (const locale of LOCALES) {
    const cs = JSON.parse(readFileSync(join(here, '..', '..', 'i18n', `${locale}.json`), 'utf8')).code_studio;
    const facts = new Set([cs.node_container_yes, cs.node_container_no, cs.node_container_unknown]);
    assert.equal(facts.size, 3, `${locale}.json collapsed the container tri-state into fewer than three facts`);
    assert.notEqual(cs.mode_container_remote, cs.mode_container_unavailable,
      `${locale}.json: silence and "no runtime" must not read alike`);
  }
});

test('the GUI-session sentence is its own message, not a copy of the generic one', () => {
  for (const locale of LOCALES) {
    const cs = JSON.parse(readFileSync(join(here, '..', '..', 'i18n', `${locale}.json`), 'utf8')).code_studio;
    assert.notEqual(cs.mode_process_gui_session, cs.mode_process_unavailable,
      `${locale}.json: the GUI-session cause must say more than "unavailable"`);
    assert.notEqual(cs.mode_process_gui_session, cs.mode_process_remote);
  }
});
