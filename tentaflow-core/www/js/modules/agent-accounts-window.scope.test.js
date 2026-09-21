// =============================================================================
// File: modules/agent-accounts-window.scope.test.js
// Description: A03 prints the account's sessions, but `provider_account_sessions`
//       is runtime state and does NOT replicate: `dispatch/provider_account.rs`
//       answers with the rows THIS node holds. For an account homed elsewhere
//       that subset used to be rendered under the account's own heading with an
//       empty table reading "Brak aktywnych sesji." — a statement about a
//       machine this node cannot see, and §H.4 of the technical design requires
//       "brak danych" when that node is offline instead.
//
//       The shipped module is IMPORTED, not sliced out of the file text: the
//       `/js/` resolver is installed process-wide by `js/_test-register.js`, and
//       a module that pulls `tf-tabs`/`tf-table` imports under it like any other
//       (`tentanas.test.js` does the same).
//
//       What is covered, and what each drift breaks:
//
//       - the PRODUCTION call site, driven end to end: `mount` of
//         `modules/services/agent-accounts-tab.js`, the row the tab's OWN
//         `tf-table` produced (read out of its shadow root and clicked there, so
//         `_onClick` builds `row-click` from the object `paintAccounts` wrote),
//         the tab's own row-click listener, then the DOM the window built. The
//         tab really runs its load path (it asks `AgentAccounts.runtimeNodes`
//         and keeps the answer; `AgentAccounts` is the transport boundary),
//         and the row PAYLOAD is covered and not merely the call argument — the
//         `AgentAccounts.get` stub answers only the id the row carries, so
//         dropping `_accountId: account.account_id` from `paintAccounts` fails
//         both cases with "the click on the tab's own row must open the account
//         that row names". Removing `runtimeNodes: state.nodes` from
//         `openAccount` fails the same two "the tab hands the window the matrix
//         it read" cases.
//       - the internal call inside `agent-accounts-window.js`. Dropping
//         `runtimeNodes` there fails the remote-home cases and renders the
//         honest "Brak danych." instead (a home with no row counts as not
//         answering); dropping `localNodeId` there fails five cases and renders
//         the honest "Zakres nieznany." (an unnamed machine is unknown, and no
//         account is then claimed to live here). Neither drift can bring the
//         false wording back: "Brak aktywnych sesji." is now reachable from ONE
//         branch, an account homed HERE or never homed at all, and both cases are
//         asserted below — including the fifth state, where a home was recorded
//         and then deleted from the registry (`home_lost_node_id`), which reads
//         "Brak danych." and names the node that left.
//       - the pane's `empty-message`, the scoped caveat and the node marker:
//         asserted from the rendered DOM.
// =============================================================================

import '/js/sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';

const WWW_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const { window } = await import('/js/sdk-runtime/_dom-test-harness.js');

// The harness serves `file:` URLs (the wasm codec) from disk. The locale files
// and the component stylesheets are served the same way: the window under test
// builds `tf-tabs`/`tf-table`/`tf-combobox`, whose shared-style layer loads its
// sheets by served path, and a raw relative URL has no base under Node.
const harnessFetch = globalThis.fetch;
globalThis.fetch = (url, init) => {
  const href = String(url);
  const locale = /^\/i18n\/(\w+)\.json$/.exec(href);
  if (locale) {
    const text = readFileSync(join(WWW_ROOT, 'i18n', `${locale[1]}.json`), 'utf8');
    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(JSON.parse(text)) });
  }
  const path = href.startsWith('/') ? href : (href.startsWith('http') ? new URL(href).pathname : '');
  if (path.startsWith('/css/')) {
    return Promise.resolve(new window.Response(readFileSync(join(WWW_ROOT, path), 'utf8'), {
      headers: { 'Content-Type': 'text/css' },
    }));
  }
  return harnessFetch(url, init);
};
if (typeof globalThis.localStorage === 'undefined') {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
  };
}
// The globals the harness does not export but the shipped components reach for:
// `TfWindow.open` resolves through a MutationObserver, `shared-styles.js`
// feature-detects Constructable Stylesheets through the bare `Document` /
// `CSSStyleSheet` constructors from a deferred task, and layout code measures
// with a ResizeObserver. Without them the window still renders, and then throws
// after the test ends, which the runner reports as leaked asynchronous activity.
globalThis.MutationObserver ??= window.MutationObserver;
globalThis.ResizeObserver ??= window.ResizeObserver;
globalThis.Document ??= window.Document;
globalThis.CSSStyleSheet ??= window.CSSStyleSheet;

// Switching the language tells Core about the preference, which would open the
// dashboard's WebSocket and leave it reconnecting for as long as the process
// lives. `AgentAccounts.get` is replaced below for the same reason: the wiring
// under test is the window, not the transport.
const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
ApiBinary.action = () => Promise.resolve({});

const { I18n } = await import('/js/i18n.js');
await I18n.setLanguage('pl');
const pl = JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', 'pl.json'), 'utf8')).agent_accounts;
const { AgentAccounts } = await import('/js/modules/agent-accounts.js');
const { localNodeIdOf, sessionScopePresentation, openAccountWindow } = await import(
  '/js/modules/agent-accounts-window.js'
);
const { mount: mountTab, unmount: unmountTab } = await import(
  '/js/modules/services/agent-accounts-tab.js'
);

// `I18n.t` on a key that is not in the bundle returns the key PATH, so a
// message asserted against the bundle fails loudly instead of matching itself.
const fill = (template, vars) => template.replace(/\{(\w+)\}/g, (_, k) => String(vars[k]));

const LOCAL = 'helios';
const REMOTE = 'rig26';
const REMOTE_ID = 'a3f1c07d5b28e64f90d1ab34c7e5082f6b1d9a40e3c75f8216ba4d09e7c31f58';

// The matrix the window is handed. Only the answering node marks its own row;
// every other row is identified by its id, which is what `home_node_id` holds.
const matrix = (homeId, { online = true, includeHome = true } = {}) => [
  { node_id: LOCAL, node_name: 'laptop', is_local: true, online: true },
  ...(includeHome ? [{ node_id: homeId, node_name: '', is_local: false, online }] : []),
];

// ---------------------------------------------------------------------------
// localNodeIdOf — the matrix mark is the only thing that names this machine
// ---------------------------------------------------------------------------

const localId = (nodes) => localNodeIdOf(nodes);

test('localNodeIdOf: the marked row is this machine and an unmarked matrix names none', () => {
  assert.equal(localId([{ node_id: REMOTE, is_local: false }, { node_id: LOCAL, is_local: true }]), LOCAL);
  assert.equal(localId([{ node_id: REMOTE, is_local: false }]), '', 'an unmarked matrix names no machine');
  assert.equal(localId([]), '');
  assert.equal(localId(undefined), '');
});

test('localNodeIdOf: the camelCase payload spelling is read too', () => {
  assert.equal(localId([{ nodeId: LOCAL, isLocal: true }]), LOCAL);
});

// ---------------------------------------------------------------------------
// sessionScopePresentation — one rendering per scope the window can prove
// ---------------------------------------------------------------------------

const scope = (account, sessionCount, runtimeNodes) => sessionScopePresentation({
  account, sessionCount, localNodeId: localNodeIdOf(runtimeNodes), runtimeNodes,
});

test('a home node that is absent or is this machine keeps the wording it always had', () => {
  const nodes = matrix(REMOTE_ID);
  const noHome = scope({ home_node_id: null }, 0, nodes);
  assert.equal(noHome.scope, 'home');
  assert.equal(noHome.title, fill(pl.sessions_title, { count: 0 }));
  assert.equal(noHome.empty, pl.sessions_empty);
  assert.equal(noHome.note, '', 'nothing is claimed about sessions this node can see');

  const here = scope({ home_node_id: LOCAL, home_node_name: 'helios' }, 3, nodes);
  assert.equal(here.scope, 'home');
  assert.equal(here.title, fill(pl.sessions_title, { count: 3 }));
  assert.equal(here.empty, pl.sessions_empty);
  assert.equal(here.note, '');
});

test('a home node elsewhere with no naming still reports as the subset it is', () => {
  const unnamed = 'abcdef0123456789';
  const view = scope({ home_node_id: unnamed }, 0, matrix(unnamed));
  assert.equal(view.scope, 'remote');
  assert.equal(view.title, fill(pl.sessions_title_scope, { count: 0 }));
  assert.equal(view.empty, pl.sessions_remote_empty, 'never "no active sessions" about another node');
  assert.equal(view.note, fill(pl.sessions_remote_home, { node: 'abcdef01' }));
});

test('a home node whose resolved name IS its id is shortened, not spelled out in full', () => {
  // What the server sends for a node it has no display name for: the id. Sixty
  // four hex characters in a sentence name nothing.
  const view = scope(
    { home_node_id: REMOTE_ID, home_node_name: REMOTE_ID },
    0,
    matrix(REMOTE_ID),
  );
  assert.equal(view.scope, 'remote');
  assert.equal(view.note, fill(pl.sessions_remote_home, { node: 'a3f1c07d' }));
});

test('the camelCase account spelling is read too', () => {
  const view = scope({ homeNodeId: REMOTE, homeNodeName: REMOTE }, 0, matrix(REMOTE));
  assert.equal(view.scope, 'remote');
  assert.equal(view.note, fill(pl.sessions_remote_home, { node: REMOTE }));
});

test('a home node that does not answer is "brak danych", not an empty list', () => {
  for (const nodes of [matrix(REMOTE, { online: false }), matrix(REMOTE, { includeHome: false })]) {
    const view = scope({ home_node_id: REMOTE, home_node_name: REMOTE }, 0, nodes);
    assert.equal(view.scope, 'offline', `a node with no "online" row has not answered: ${JSON.stringify(nodes)}`);
    assert.equal(view.title, fill(pl.sessions_title_scope, { count: 0 }));
    assert.equal(view.empty, pl.sessions_no_data, 'design §H.4: no data, never "there are none"');
    assert.notEqual(view.empty, pl.sessions_empty);
    assert.equal(view.note, fill(pl.sessions_remote_offline, { node: REMOTE }), 'and it names the node');
  }
});

test('a window with no matrix at hand calls the scope unknown, not "homed here"', () => {
  // The row for the home node is there and online, so the ONLY thing missing is
  // the mark naming this machine — which is not enough to call the account
  // remote either.
  const nodes = [{ node_id: REMOTE, node_name: REMOTE, is_local: false, online: true }];
  const view = scope({ home_node_id: REMOTE, home_node_name: REMOTE }, 0, nodes);
  assert.equal(view.scope, 'unknown');
  assert.equal(view.empty, pl.sessions_scope_unknown);
  assert.notEqual(view.empty, pl.sessions_empty, 'the unscoped claim is not available to this state');
  assert.notEqual(view.empty, pl.sessions_remote_empty, 'and neither is the claim that this node holds none');
  assert.equal(view.note, fill(pl.sessions_scope_unknown_note, { node: REMOTE }));
});

test('a home that was DELETED from the registry is its own state, not "no home"', () => {
  // `forget_node_tx` NULLs the home and records which node it was in
  // `home_lost_node_id`. The node left the registry over expired trust, not
  // because it stopped, so its sessions may still be running — and the empty
  // cell must not claim there are none.
  const view = scope({ home_node_id: null, home_lost_node_id: REMOTE_ID }, 0, matrix(REMOTE_ID));
  assert.equal(view.scope, 'lost');
  assert.equal(view.title, fill(pl.sessions_title_scope, { count: 0 }), 'the count is this node\'s, and the title says so');
  assert.equal(view.empty, pl.sessions_no_data, 'no data, never "Brak aktywnych sesji."');
  assert.notEqual(view.empty, pl.sessions_empty);
  assert.equal(
    view.note,
    fill(pl.sessions_home_lost, { node: 'a3f1c07d' }),
    'the note names the deleted node by its short id — nothing can resolve a name for it — and says what restores a home',
  );

  // The marker is written and cleared with the home it belongs to, so it is read
  // only while there is no home: a stale value must not outrank a home this node
  // can see, in either direction of "here" or "elsewhere".
  const rehomed = scope({ home_node_id: LOCAL, home_lost_node_id: REMOTE_ID }, 0, matrix(REMOTE_ID));
  assert.equal(rehomed.scope, 'home', 'a claimed home wins over the record of a lost one');
  assert.equal(rehomed.empty, pl.sessions_empty);

  // A node id that is not a uuid is still shortened, never spelled out.
  assert.equal(
    scope({ home_lost_node_id: 'helios' }, 0, matrix(REMOTE_ID)).note,
    fill(pl.sessions_home_lost, { node: 'helios' }),
  );
});

test('the lost-home state is NOT what an account that never had a home renders', () => {
  // This distinction is the whole point of the marker: an account that was never
  // homed runs wherever it is first used, so this node's list IS its list, and
  // the caveat must not appear. Only a recorded loss can produce it.
  for (const never of [
    { home_node_id: null },
    { home_node_id: null, home_lost_node_id: null },
    { home_node_id: null, home_lost_node_id: '' },
    { home_node_id: null, home_lost_node_id: undefined },
  ]) {
    const view = scope(never, 0, matrix(REMOTE_ID));
    assert.equal(view.scope, 'home', `never homed: ${JSON.stringify(never)}`);
    assert.equal(view.empty, pl.sessions_empty);
    assert.equal(view.note, '');
  }
});


// ---------------------------------------------------------------------------
// The call sites: what `openAccountWindow` actually renders
//
// These are the assertions that fail when the wiring is unpicked, because the
// rendering is read back out of the DOM the shipped window built — replacing
// `AgentAccounts.get` with a fixed answer is the only thing stubbed.
// ---------------------------------------------------------------------------

const SESSIONS = '[data-table="sessions"]';
const NODES = '[data-table="nodes"]';
const SCOPE_NOTE = '[data-hint="sessions-scope"]';

function answer({
  homeNodeId = null, homeNodeName = '', homeLostNodeId = null, sessions = [], nodes = null,
} = {}) {
  return {
    account: {
      account_id: 'acct-1',
      display_name: 'Konto',
      scope: 'global',
      engine_id: 'codex',
      status: 'active',
      credential_kind: 'provider_login',
      credential_revision: 1,
      home_node_id: homeNodeId,
      home_node_name: homeNodeName,
      // What `forget_node_tx` writes when the home's node is deleted: the id of
      // the node that WAS the home, with no name beside it — a deleted node has
      // none to resolve.
      home_lost_node_id: homeLostNodeId,
    },
    grants: [],
    sessions,
    // The account's per-node state, as the server measured it on THIS node —
    // which is not the matrix and says nothing about which row is this machine.
    nodes: nodes ?? [{ node_id: homeNodeId ?? LOCAL, node_name: homeNodeName, last_error: null }],
    agents: [],
  };
}

// Opens A03 against the fixed answer and returns the elements the state is read
// from. `runtimeNodes` is what the tab hands the window in production.
async function open({ payload, runtimeNodes }) {
  document.body.innerHTML = '';
  AgentAccounts.get = async () => payload;
  await openAccountWindow('acct-1', { engines: [], runtimeNodes, isAdmin: false });
  const body = document.querySelector('.aa-detail');
  assert.ok(body, 'the window opened');
  const headings = [...body.querySelectorAll('h4.aa-sub-h')].map((el) => el.textContent);
  const table = body.querySelector(SESSIONS);
  assert.ok(table, 'the sessions table is rendered');
  return {
    body,
    headings,
    empty: table.getAttribute('empty-message'),
    note: body.querySelector(SCOPE_NOTE)?.textContent ?? '',
    nodeRows: body.querySelector(NODES).rows,
  };
}

test('A03: an account homed here renders the unscoped wording, with no caveat', async () => {
  const view = await open({
    payload: answer({ homeNodeId: LOCAL, homeNodeName: 'helios', sessions: [{ session_id: 's1' }] }),
    runtimeNodes: matrix(REMOTE_ID),
  });
  assert.equal(view.headings[1], fill(pl.sessions_title, { count: 1 }), `sessions heading: ${view.headings.join(' | ')}`);
  assert.equal(view.empty, pl.sessions_empty);
  assert.equal(view.note, '', 'the scoped note must not appear for an account homed here');
});

test('A03: an account homed elsewhere names the node and scopes the heading and the cell', async () => {
  const view = await open({
    payload: answer({ homeNodeId: REMOTE_ID, homeNodeName: REMOTE, sessions: [{ session_id: 's1' }] }),
    runtimeNodes: matrix(REMOTE_ID),
  });
  assert.equal(view.headings[1], fill(pl.sessions_title_scope, { count: 1 }), 'the count is this node\'s, and the title says so');
  assert.equal(view.empty, pl.sessions_remote_empty);
  assert.equal(view.note, fill(pl.sessions_remote_home, { node: REMOTE }));
});

test('A03: a home node that stopped answering renders §H.4\'s "brak danych"', async () => {
  const view = await open({
    payload: answer({ homeNodeId: REMOTE_ID, homeNodeName: REMOTE }),
    // The same matrix, with that node's row reporting it is not online.
    runtimeNodes: matrix(REMOTE_ID, { online: false }),
  });
  assert.equal(view.empty, pl.sessions_no_data, 'the empty cell must not read as "no sessions"');
  assert.equal(view.note, fill(pl.sessions_remote_offline, { node: REMOTE }));
  assert.equal(view.headings[1], fill(pl.sessions_title_scope, { count: 0 }), 'and it still says which list this is');
});

test('A03: a tab that could not read the matrix calls the scope unknown', async () => {
  // What `modules/services/agent-accounts-tab.js` leaves behind when
  // `runtimeNodes()` is refused: an empty matrix, and no row marked as this
  // machine. The account is NOT claimed to live here, and the cell does not
  // claim there are no sessions.
  const view = await open({
    payload: answer({ homeNodeId: REMOTE_ID, homeNodeName: REMOTE }),
    runtimeNodes: [],
  });
  assert.equal(view.empty, pl.sessions_scope_unknown);
  assert.equal(view.note, fill(pl.sessions_scope_unknown_note, { node: REMOTE }));
});

test('A03: an account whose home node was deleted renders "brak danych", not an empty list', async () => {
  // The defect this state exists for: the home node's registry row was pruned
  // over expired trust while its sessions keep running, `forget_node_tx` NULLed
  // the home and recorded which node it was, and the card used to print "Brak
  // aktywnych sesji." over a list this node cannot read.
  const view = await open({
    payload: answer({ homeLostNodeId: REMOTE_ID }),
    runtimeNodes: matrix(REMOTE_ID),
  });
  assert.equal(view.empty, pl.sessions_no_data, 'the cell must not claim there are no sessions');
  assert.notEqual(view.empty, pl.sessions_empty);
  assert.equal(
    view.note,
    fill(pl.sessions_home_lost, { node: 'a3f1c07d' }),
    'the caveat names the deleted node by its short id and says what restores a home',
  );
  assert.equal(view.headings[1], fill(pl.sessions_title_scope, { count: 0 }), 'and it still says which list this is');
});

test('A03: an account that was never homed keeps the unscoped wording', async () => {
  // The other half of the distinction, read off the same DOM: no home and no
  // marker is an account that runs wherever it is first used, so this node's
  // list IS its list and no caveat belongs over it.
  const view = await open({ payload: answer(), runtimeNodes: matrix(REMOTE_ID) });
  assert.equal(view.empty, pl.sessions_empty);
  assert.equal(view.note, '', 'a never-homed account carries no caveat');
});

test('A03: the node table marks the answering machine and no peer', async () => {
  const view = await open({
    payload: answer({
      homeNodeId: REMOTE_ID,
      homeNodeName: REMOTE,
      // Both nodes hold the credential, so both appear in "Na nodach" — the
      // marker has to pick out the one this window is looking at.
      nodes: [
        { node_id: LOCAL, node_name: 'laptop', last_error: null },
        { node_id: REMOTE_ID, node_name: REMOTE, last_error: null },
      ],
    }),
    runtimeNodes: [
      { node_id: LOCAL, node_name: 'laptop', is_local: true, online: true },
      { node_id: REMOTE_ID, node_name: REMOTE, is_local: false, online: true },
    ],
  });
  assert.equal(view.nodeRows.length, 2, `both nodes are listed: ${JSON.stringify(view.nodeRows.map((r) => r.node))}`);
  const marked = view.nodeRows.filter((row) => String(row.node).includes(pl.node_local));
  assert.deepEqual(
    marked.map((row) => String(row.node).includes('laptop')),
    [true],
    `exactly the local row carries the "this node" line: ${JSON.stringify(view.nodeRows.map((r) => r.node))}`,
  );
});

// ---------------------------------------------------------------------------
// The PRODUCTION call site: the matrix the TAB read reaches the window
//
// The matrix only exists in the tab's own state, so the window is honest about
// an account homed elsewhere only if `openAccount` hands `state.nodes` over.
// Nothing in the window is stubbed here: `mount` runs the tab's real load path,
// the row-click listener is the one `tf-table` fires, and `AgentAccounts` — the
// transport — is the boundary that is replaced.
// ---------------------------------------------------------------------------

async function waitForWindow() {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    const body = document.querySelector('.aa-detail');
    if (body) return body;
    await new Promise((resolve) => { setTimeout(resolve, 5); });
  }
  throw new Error('the accounts tab never opened the account window');
}

async function openThroughTab({ homeNodeId, homeNodeName, homeLostNodeId = null, runtimeNodes }) {
  document.body.innerHTML = '';
  const host = document.createElement('div');
  document.body.appendChild(host);
  AgentAccounts.list = async () => ({
    accounts: [{
      account_id: 'acct-1',
      display_name: 'Konto',
      scope: 'global',
      engine_id: 'codex',
      credential_kind: 'provider_login',
      credential_revision: 1,
      status: 'active',
      home_node_id: homeNodeId,
      home_node_name: homeNodeName,
    }],
    engines: [],
  });
  AgentAccounts.runtimeNodes = async () => ({ nodes: runtimeNodes });
  // The transport boundary, and the one stub that is deliberately narrow: the
  // real request addresses ONE account, so an id the tab did not take from its
  // own row is refused rather than answered with the fixed payload. `asked`
  // records what the click handed over, so the row's own id is an assertion and
  // not a detail the fixed answer would hide.
  const asked = [];
  AgentAccounts.get = async (accountId) => {
    asked.push(accountId);
    if (accountId !== 'acct-1') throw new Error(`no account '${String(accountId)}'`);
    return answer({ homeNodeId, homeNodeName, homeLostNodeId });
  };
  try {
    await mountTab(host, { isAdmin: true });
    const table = host.querySelector('#aa-accounts-table');
    assert.ok(table, 'the tab rendered its accounts table');
    // The clicked element is the one the tab's OWN `tf-table` produced: the row
    // is read out of the table's shadow root and clicked there, so `_onClick`
    // builds `row-click` from the row object `paintAccounts` wrote. A
    // synthesized event carrying a hand-written row would stay green while the
    // tab's own row lost the fields its listener reads.
    const cell = table.shadowRoot?.querySelector('tbody tr td');
    assert.ok(cell, 'the tab\'s accounts table rendered a data row');
    cell.dispatchEvent(new MouseEvent('click', { bubbles: true, composed: true }));
    assert.deepEqual(asked, ['acct-1'],
      'the click on the tab\'s own row must open the account that row names');
    const body = await waitForWindow();
    const sessionsTable = body.querySelector(SESSIONS);
    assert.ok(sessionsTable, 'the window rendered the sessions table');
    return {
      body,
      empty: sessionsTable.getAttribute('empty-message'),
      note: body.querySelector(SCOPE_NOTE)?.textContent ?? '',
    };
  } finally {
    unmountTab();
  }
}

test('the tab hands over its matrix: an account homed on this machine is not called remote', async () => {
  const view = await openThroughTab({
    homeNodeId: LOCAL,
    homeNodeName: 'helios',
    runtimeNodes: matrix(REMOTE_ID),
  });
  assert.equal(view.empty, pl.sessions_empty,
    'the window must be handed the matrix the tab read (`runtimeNodes: state.nodes`) — with no matrix this machine is unnamed and the cell reads "Zakres nieznany." instead');
  assert.equal(view.note, '', 'and an account homed here carries no caveat');
});

test('the tab hands over its matrix: a home node that answers is named from it', async () => {
  const view = await openThroughTab({
    homeNodeId: REMOTE_ID,
    homeNodeName: REMOTE,
    runtimeNodes: matrix(REMOTE_ID),
  });
  assert.equal(view.empty, pl.sessions_remote_empty,
    'the window must be handed the matrix the tab read (`runtimeNodes: state.nodes`) and pass it on — with no matrix the home node is not placed and the cell does not read "Ten node nie prowadzi sesji tego konta."');
  assert.equal(view.note, fill(pl.sessions_remote_home, { node: REMOTE }),
    'and the caveat names the node the tab saw as online');
});

test('the tab hands over its matrix: a deleted home reaches the cell through the real row click', async () => {
  // The whole path, from the tab's own row to the rendered DOM, with the lost
  // home arriving where it really arrives — in the account payload, not in the
  // matrix, because the node it names is gone from that matrix.
  const view = await openThroughTab({
    homeNodeId: null,
    homeNodeName: '',
    homeLostNodeId: REMOTE_ID,
    runtimeNodes: matrix(REMOTE_ID),
  });
  assert.equal(view.empty, pl.sessions_no_data,
    'an account whose home was deleted must not read as "no active sessions" through the tab either');
  assert.notEqual(view.empty, pl.sessions_empty);
  assert.equal(view.note, fill(pl.sessions_home_lost, { node: 'a3f1c07d' }));
});

// ---------------------------------------------------------------------------
// The keys these branches ask for must exist everywhere the dashboard speaks
// ---------------------------------------------------------------------------

const BRANCH_KEYS = [
  'sessions_title', 'sessions_empty', 'sessions_title_scope', 'sessions_remote_empty',
  'sessions_remote_home', 'sessions_remote_offline', 'sessions_no_data',
  'sessions_scope_unknown', 'sessions_scope_unknown_note', 'sessions_home_lost', 'node_local',
];

test('every locale translates every key the five scopes can ask for', () => {
  for (const locale of LOCALES) {
    const bundle = JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${locale}.json`), 'utf8'))
      .agent_accounts;
    for (const key of BRANCH_KEYS) {
      assert.equal(typeof bundle[key], 'string', `${locale}.json is missing agent_accounts.${key}`);
      assert.ok(bundle[key].trim().length > 0, `${locale}.json has an empty agent_accounts.${key}`);
    }
    assert.ok(bundle.sessions_title_scope.includes('{count}'),
      `${locale}.json: the scoped title is the one that carries the count`);
    for (const key of [
      'sessions_remote_home', 'sessions_remote_offline', 'sessions_scope_unknown_note',
      'sessions_home_lost',
    ]) {
      assert.ok(bundle[key].includes('{node}'),
        `${locale}.json: ${key} has to name the node the rest of the list lives on`);
    }
  }
});

test('the scoped messages are their own, not a copy of the ones they replace', () => {
  for (const locale of LOCALES) {
    const bundle = JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${locale}.json`), 'utf8'))
      .agent_accounts;
    assert.notEqual(bundle.sessions_remote_empty, bundle.sessions_empty,
      `${locale}.json: "no sessions here" must not read as "no active sessions"`);
    assert.notEqual(bundle.sessions_no_data, bundle.sessions_empty,
      `${locale}.json: "brak danych" must not read as "no active sessions"`);
    assert.notEqual(bundle.sessions_scope_unknown, bundle.sessions_empty,
      `${locale}.json: an unknown scope must not read as "no active sessions"`);
    assert.notEqual(bundle.sessions_home_lost, bundle.sessions_empty,
      `${locale}.json: a deleted home must not be described as "no active sessions"`);
    assert.notEqual(bundle.sessions_home_lost, bundle.sessions_remote_offline,
      `${locale}.json: a home that was DELETED and a home that is merely not answering are \
       different states, and the second must not stand in for the first`);
    assert.notEqual(bundle.sessions_title_scope, bundle.sessions_title,
      `${locale}.json: a scoped count must not carry the unscoped heading`);
  }
});

// Three caveats state the SCOPE, never the other node's contents, and each has
// to hedge in its own way. The two remote ones are rendered in a state where
// this node cannot tell whether the home node is running anything at all, so
// neither sentence may assert that sessions exist there — and
// `sessions_remote_offline` covers TWO shapes (a row reporting `online: false`
// and no row for the home at all), so it may not name a cause either. The
// deleted-home note must hedge differently: deleting a node from the registry
// says nothing about whether it was running anything, so "may still be running"
// is the strongest true claim and every locale has to make it that weak.
const HEDGES = {
  sessions_remote_home: {
    pl: 'jeśli', en: 'if any', de: 'falls', es: 'si las hay', fr: "s'il y en a",
  },
  sessions_remote_offline: {
    pl: 'nie wiadomo', en: 'unknown', de: 'unbekannt', es: 'no se sabe', fr: 'ignore',
  },
  sessions_home_lost: {
    pl: 'mogą', en: 'may', de: 'möglicherweise', es: 'pueden', fr: 'peuvent',
  },
};

test('the caveats hedge: they state what this screen reads, not what runs there', () => {
  for (const locale of LOCALES) {
    const bundle = JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${locale}.json`), 'utf8'))
      .agent_accounts;
    for (const [key, hedges] of Object.entries(HEDGES)) {
      assert.ok(bundle[key].includes(hedges[locale]),
        `${locale}.json: ${key} has to carry the hedge its own state requires`);
    }
  }
});
