// =============================================================================
// File: modules/code-studio-session.account.test.js
// Description: What the C01 card says and what answering it does.
//       A run parked on a missing account is the one question in the console
//       whose answer is a SIGN-IN in another window (A02): nothing is granted,
//       the PEP never sees it, and the turn continues by itself once the server
//       settles the row. So the card must (a) name the application the sign-in
//       is about, (b) offer exactly two ways out — connect, or end the task —
//       and (c) name the account mode in the account screens' own words, in all
//       five locales. The sub-agent chip (C02) is composed here too, so the
//       label the run list and the tabs show is pinned to the mockup, and the
//       console row that carries it must actually be built — once, on the
//       announcement, while the rest of the run stays in its pane.
//       The functions are not exported (the module pulls the whole dashboard
//       in), so their source is cut out of the real file and evaluated — the
//       code under test is the shipped code.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'code-studio-session.js'), 'utf8');

function cutBalanced(src, start, open, close) {
  let depth = 0;
  let i = src.indexOf(open, start);
  for (; i < src.length; i += 1) {
    if (src[i] === open) depth += 1;
    else if (src[i] === close) {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

function cutFn(name) {
  const start = source.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`no definition: ${name}`);
  return cutBalanced(source, start, '{', '}');
}

function cutConst(name) {
  const start = source.indexOf(`const ${name} = `);
  if (start < 0) throw new Error(`no constant: ${name}`);
  return source.split('\n').find((line) => line.startsWith(`const ${name} = `));
}

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'].map((lang) => [
  lang,
  JSON.parse(readFileSync(join(here, '..', '..', 'i18n', `${lang}.json`), 'utf8')),
]);

function lookup(root, key) {
  return key.split('.').reduce((node, part) => (node == null ? node : node[part]), root);
}

function fill(text, vars) {
  return String(text).replace(/\{(\w+)\}/g, (whole, name) => (
    vars && name in vars ? String(vars[name]) : whole
  ));
}

/// The shipped functions, bound to one locale's dictionary. A key the module
/// asks for and the locale does not have is a failure, not an empty string.
function ship(root) {
  const T = (key, vars) => {
    const text = lookup(root, key);
    assert.ok(typeof text === 'string', `missing translation: ${key}`);
    return fill(text, vars);
  };
  return new Function('T', `
    ${cutConst('ACCOUNT_LOGIN_CAPABILITY')}
    const APPROVAL_SCOPES = ['allow_once', 'allow_for_run', 'allow_for_session', 'always'];
    const t = (key, vars) => T('code_studio.' + key, vars);
    const I18n = { t: (key, vars) => T(key, vars) };
    // The stream rows are built as DOM nodes; the test only reads the text
    // they carry, so the helper hands the markup back as a string.
    const node = (html) => String(html);
    const sprite = (id) => '<svg data-icon="' + id + '"></svg>';
    const escapeHtml = (value) => String(value ?? '');
    const escapeAttr = (value) => String(value ?? '');
    ${cutFn('accountAskFromApproval')}
    ${cutFn('askOptions')}
    ${cutFn('askMarkNode')}
    ${cutFn('accountChipLabel')}
    return { accountAskFromApproval, askOptions, askMarkNode, accountChipLabel };
  `)(T);
}

const MODULES = new Map(LOCALES.map(([lang, root]) => [lang, ship(root)]));

// The row the server parks the run on: `suspend_for_account` writes the snapshot
// (`approval.account`) when it asks, so nothing here is read from the agent.
const SERVER_APPROVAL = {
  approval_id: 'ap-9',
  run_id: 'run-7',
  capability: 'account_login',
  summary: 'code-implementer używa Twojego konta Claude Code',
  account: {
    engine_id: 'codex',
    engine_name: 'Codex',
    mode: 'global',
    agent_name: 'code-implementer',
    account_id: null,
    account_name: '',
  },
};

// ---------------------------------------------------------------------------
// The card (C01)
// ---------------------------------------------------------------------------

test('the card names the application, the agent and the mode in the account screens’ own words', () => {
  for (const [lang, root] of LOCALES) {
    const ask = MODULES.get(lang).accountAskFromApproval(SERVER_APPROVAL);
    assert.equal(ask.kind, 'account', `${lang} card kind`);
    assert.equal(ask.capability, 'account_login', `${lang} card capability`);
    // The answer is keyed to the row and to the run: the row is what the server
    // settles, the run is what resumes.
    assert.equal(ask.approvalId, 'ap-9', `${lang} approval id`);
    assert.equal(ask.runId, 'run-7', `${lang} run id`);
    assert.equal(ask.account.engineId, 'codex');
    assert.equal(ask.account.mode, 'global');
    const mode = lookup(root, 'agent_accounts.subtitle_global');
    assert.equal(ask.who, `code-implementer · Codex · ${mode}`, `${lang} card head`);
    assert.equal(
      ask.question,
      fill(lookup(root, 'code_studio.ask.account.body'), { engine: 'Codex' }),
      `${lang} card question`,
    );
    assert.equal(ask.mandatory, false, `${lang}: a sign-in is not a switchable-off permission`);
  }
});

test('an account that lost its credential asks for a new sign-in, not for a new account', () => {
  for (const [lang, root] of LOCALES) {
    const mod = MODULES.get(lang);
    const create = mod.accountAskFromApproval(SERVER_APPROVAL);
    const again = mod.accountAskFromApproval({
      ...SERVER_APPROVAL,
      account: { ...SERVER_APPROVAL.account, account_id: 'acc-1', account_name: 'Codex — firma' },
    });
    assert.equal(again.account.accountId, 'acc-1', `${lang} existing account id`);
    assert.equal(
      again.question,
      fill(lookup(root, 'code_studio.ask.account.body_relogin'), { engine: 'Codex' }),
      `${lang} relogin question`,
    );
    assert.notEqual(create.question, again.question, `${lang}: the two situations read alike`);
  }
});

test('the two ways out are a sign-in and an end to the task — never a permission scope', () => {
  for (const [lang, root] of LOCALES) {
    const mod = MODULES.get(lang);
    const opts = mod.askOptions(mod.accountAskFromApproval(SERVER_APPROVAL));
    assert.equal(opts.length, 2, `${lang}: the account card asks one question, not a scope list`);
    assert.deepEqual(opts.map((o) => o.action), ['answer-login', 'answer-deny'], `${lang} options`);
    assert.deepEqual(opts.map((o) => o.key), ['1', '2'], `${lang} option markers`);
    assert.equal(
      opts[0].label,
      fill(lookup(root, 'code_studio.ask.account.connect'), { engine: 'Codex' }),
      `${lang} connect row`,
    );
    assert.equal(opts[1].label, lookup(root, 'code_studio.ask.account.cancel'), `${lang} cancel row`);
    assert.ok(opts.every((o) => o.detail), `${lang}: a row with no explanation is a guess`);
    // A scope value would make the console send a grant for a capability the
    // PEP never minted.
    assert.ok(opts.every((o) => !o.value), `${lang}: an account answer grants nothing`);
  }
});

// ---------------------------------------------------------------------------
// The anchor left in the stream
// ---------------------------------------------------------------------------

test('the line in the stream says an account is missing, not that a permission is', () => {
  for (const [lang, root] of LOCALES) {
    const mod = MODULES.get(lang);
    const account = mod.askMarkNode({
      p: { capability: 'account_login', approval_id: 'ap-9', summary: 'busy' },
    });
    assert.ok(
      account.includes(lookup(root, 'code_studio.ask.account.anchor')),
      `${lang}: the account line does not anchor the question`,
    );
    assert.ok(account.includes(lookup(root, 'code_studio.ask.account.go')), `${lang} account go`);
    assert.ok(
      !account.includes(lookup(root, 'code_studio.ask.anchor')),
      `${lang}: an account question still reads as a permission request`,
    );

    const permission = mod.askMarkNode({
      p: { capability: 'git_push', approval_id: 'ap-10', summary: 'git push' },
    });
    assert.ok(permission.includes(lookup(root, 'code_studio.ask.anchor')), `${lang} permission anchor`);
    assert.ok(permission.includes(lookup(root, 'code_studio.ask.go')), `${lang} permission go`);
    assert.ok(
      !permission.includes(lookup(root, 'code_studio.ask.account.anchor')),
      `${lang}: a permission reads as an account question`,
    );
  }
});

// ---------------------------------------------------------------------------
// The sub-agent chip (C02)
// ---------------------------------------------------------------------------

test('the chip names the engine and the mode, and the account when it is known', () => {
  const cases = [
    [{ engine_name: 'Codex', account_name: 'Codex — firma', mode: 'global' }, true],
    [{ engine_name: 'Codex', account_name: '', mode: 'user' }, false],
  ];
  for (const [lang, root] of LOCALES) {
    const mod = MODULES.get(lang);
    const globalMode = lookup(root, 'agent_accounts.subtitle_global');
    const userMode = lookup(root, 'agent_accounts.subtitle_user');
    assert.equal(
      mod.accountChipLabel(cases[0][0]),
      fill(lookup(root, 'code_studio.session.account_chip'), {
        engine: 'Codex', mode: globalMode, account: 'Codex — firma',
      }),
      `${lang} named chip`,
    );
    assert.equal(
      mod.accountChipLabel(cases[1][0]),
      fill(lookup(root, 'code_studio.session.account_chip_plain'), { engine: 'Codex', mode: userMode }),
      `${lang} unnamed chip`,
    );
    // A run the node has no account for must not print a chip at all: the
    // caller passes '' and the tab drops the attribute.
    assert.equal(mod.accountChipLabel(null), '', `${lang} no account, no chip`);
  }
});

// ---------------------------------------------------------------------------
// The words themselves
// ---------------------------------------------------------------------------

test('every word of the card exists in all five locales with the placeholders it is called with', () => {
  const keys = {
    'ask.account.head': [],
    'ask.account.anchor': [],
    'ask.account.go': [],
    'ask.account.body': ['engine'],
    'ask.account.body_relogin': ['engine'],
    'ask.account.connect': ['engine'],
    'ask.account.connect_detail': [],
    'ask.account.cancel': [],
    'ask.account.cancel_detail': [],
    'session.account_chip': ['engine', 'mode', 'account'],
    'session.account_chip_plain': ['engine', 'mode'],
  };
  for (const [lang, root] of LOCALES) {
    for (const [key, vars] of Object.entries(keys)) {
      const text = lookup(root, `code_studio.${key}`);
      assert.ok(typeof text === 'string' && text.length, `${lang} is missing code_studio.${key}`);
      for (const name of vars) {
        assert.ok(text.includes(`{${name}}`), `${lang} code_studio.${key} does not carry {${name}}`);
      }
    }
    // The mode on the chip is the account screens' own wording, so the same
    // account reads alike on My accounts, in the run list and on the tab.
    for (const key of ['subtitle_global', 'subtitle_user']) {
      assert.ok(lookup(root, `agent_accounts.${key}`), `${lang} is missing agent_accounts.${key}`);
    }
  }
});

// ---------------------------------------------------------------------------
// The spawn announcement (C02)
// ---------------------------------------------------------------------------
//
// The chip above only matters if the console row it sits on is ever built. A
// sub-agent's events belong to its own pane, but the ONE line announcing the run
// belongs to the console; the mockup's `ev-spawn` row. Which stream an event is
// appended to is locale-independent, so these run against `en`.

/// `ingestEvents` with its nervous system stubbed: appends are recorded instead
/// of drawn, and the spawn row goes through the SHIPPED `runStartedNode`.
function ingestWorld(locale, runs) {
  const appends = [];
  const state = {
    seen: new Set(),
    cursor: 0,
    events: [],
    eventsByRun: new Map(),
    subagentRuns: new Set(),
    openRunId: '',
    runs,
    turnOrdinal: 0,
  };
  const T = (key, vars) => {
    const text = lookup(locale, key);
    return text === undefined ? key : fill(text, vars);
  };
  const mod = new Function('T', 'state', 'APPENDS', `
    const t = (key, vars) => T('code_studio.' + key, vars);
    const I18n = { t: (key, vars) => T(key, vars) };
    const EVENT_BUFFER = 500;
    const node = (html) => String(html);
    const sprite = (id) => '<svg data-icon="' + id + '"></svg>';
    const escapeHtml = (value) => String(value ?? '');
    const escapeAttr = (value) => String(value ?? '');
    const shortId = (id) => String(id || '');
    const clockOf = () => '';
    const appendNode = (stream, child) => APPENDS.push({ stream, node: String(child) });
    const streamEl = (name) => name;
    const feedActivity = () => {};
    const reactToEvent = () => {};
    const refreshSide = () => {};
    const paintDockEmpty = () => {};
    const updateCounters = () => {};
    ${cutFn('turnNode')}
    ${cutFn('accountChipLabel')}
    ${cutFn('normalizeEvent')}
    ${cutFn('classifyRun')}
    ${cutFn('runStartedNode')}
    function buildEventNode(ev, scope) {
      return ev.kind === 'run_started' ? runStartedNode(ev, scope) : scope + ':' + ev.kind;
    }
    ${cutFn('ingestEvents')}
    return { ingestEvents };
  `)(T, state, appends);
  return { ...mod, state, appends };
}

function evt(seq, kind, runId, payload, agentId = '') {
  return {
    seq,
    event_id: `e${seq}`,
    kind,
    run_id: runId,
    agent_id: agentId,
    created_at: '2026-09-17T10:00:00Z',
    payload_json: JSON.stringify(payload),
  };
}

const SPAWNED_RUN = {
  run_id: 'run-sub',
  kind: 'subagent',
  account: { engine_name: 'Codex', account_name: 'Codex — firma', mode: 'global' },
};

test('a sub-agent spawn is announced once in the console and its later events are not', () => {
  const [, root] = LOCALES.find(([lang]) => lang === 'en');
  for (const kind of ['subagent', 'cli']) {
    const w = ingestWorld(root, [{ ...SPAWNED_RUN, kind }]);
    w.ingestEvents([
      evt(1, 'run_started', 'run-sub', { kind, trigger: 'user' }, 'code-planner'),
      evt(2, 'agent_message', 'run-sub', { role: 'assistant', text: 'planning' }),
      evt(3, 'run_finished', 'run-sub', { status: 'done' }),
    ]);
    const rows = w.appends.filter((a) => a.stream === 'console');
    assert.equal(rows.length, 1, `${kind}: the console got more than the one announcement`);
    assert.ok(rows[0].node.includes('ev-spawn'), `${kind}: the console row is not the spawn line`);
    assert.ok(rows[0].node.includes('code-planner'), `${kind}: the spawn line does not name the agent`);
    assert.ok(rows[0].node.includes('tf-chip'), `${kind}: the account chip is missing`);
    assert.ok(rows[0].node.includes('Codex — firma'), `${kind}: the chip does not name the account`);
  }
});

test('a run the page already knows about still announces itself when the timeline replays', () => {
  const [, root] = LOCALES.find(([lang]) => lang === 'en');
  const w = ingestWorld(root, [SPAWNED_RUN]);
  // `bootstrap()` loads the runs BEFORE the timeline, so on a reload the set
  // already holds the run by the time its `run_started` is replayed.
  w.state.subagentRuns.add('run-sub');
  w.ingestEvents([evt(1, 'run_started', 'run-sub', { kind: 'subagent', trigger: 'user' }, 'code-planner')]);
  const rows = w.appends.filter((a) => a.stream === 'console');
  assert.equal(rows.length, 1, 'a known run lost its announcement');
  assert.ok(rows[0].node.includes('ev-spawn'), 'the replayed row is not the spawn line');
  assert.ok(rows[0].node.includes('Codex — firma'), 'the replayed chip does not name the account');
});

test('the announcement is not doubled when its pane is open or the poll repeats the event', () => {
  const [, root] = LOCALES.find(([lang]) => lang === 'en');
  const w = ingestWorld(root, [SPAWNED_RUN]);
  const spawn = evt(1, 'run_started', 'run-sub', { kind: 'subagent', trigger: 'user' }, 'code-planner');
  w.state.openRunId = 'run-sub';
  w.ingestEvents([spawn]);
  w.ingestEvents([spawn]);
  const consoleRows = w.appends.filter((a) => a.stream === 'console');
  const paneRows = w.appends.filter((a) => a.stream === 'subagent');
  assert.equal(consoleRows.length, 1, 'the console shows the spawn more than once');
  assert.equal(paneRows.length, 1, 'the open pane did not get the run start');
  assert.ok(!paneRows[0].node.includes('ev-spawn'), 'the pane renders the console row as well');
});

test('a root run still opens the console with a turn, not a spawn row', () => {
  const [, root] = LOCALES.find(([lang]) => lang === 'en');
  const w = ingestWorld(root, [{ run_id: 'run-root', kind: 'root', account: null }]);
  w.ingestEvents([evt(1, 'run_started', 'run-root', { kind: 'root', trigger: 'user' })]);
  const rows = w.appends.filter((a) => a.stream === 'console');
  assert.equal(rows.length, 1, 'the root run did not open the console once');
  assert.ok(!rows[0].node.includes('ev-spawn'), 'a root run was announced as a sub-agent');
});

test('the line announces itself before the run row is polled in, and then carries no chip', () => {
  const [, root] = LOCALES.find(([lang]) => lang === 'en');
  // `loadRuns` may not have reported the run yet when its `run_started` arrives;
  // the account is a fact about the run row, so the line waits for it.
  const w = ingestWorld(root, []);
  w.ingestEvents([evt(1, 'run_started', 'run-sub', { kind: 'subagent', trigger: 'user' }, 'code-planner')]);
  const rows = w.appends.filter((a) => a.stream === 'console');
  assert.equal(rows.length, 1, 'the announcement was swallowed without a run row');
  assert.ok(rows[0].node.includes('ev-spawn'), 'the row is not the spawn line');
  assert.ok(!rows[0].node.includes('tf-chip'), 'a chip was invented for an unknown account');
});
