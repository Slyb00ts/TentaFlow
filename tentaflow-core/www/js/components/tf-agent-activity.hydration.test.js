// =============================================================================
// File: components/tf-agent-activity.hydration.test.js
// Description: Tests for the run row of <tf-agent-activity> — what it says a
//       run cost. The widget used to live on live events alone, so a session
//       console opened after the turn had ended dated every run from the moment
//       the client happened to look ("root 0s · 0 tokens" for a turn that took
//       74 seconds and spent 7434). These pin the hydration path: a persisted
//       row wins over observation time, a finished run is measured between its
//       own two timestamps, and a bare status update may not invent a row.
//       The module imports its tf-* siblings by absolute URL, which Node cannot
//       resolve; those imports only register elements, so the shipped source is
//       loaded with them stripped.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'tf-agent-activity.js'), 'utf8')
  .replace(/^import\s+'[^']*';$/gm, '');
const { TfAgentActivity } = await import(
  `data:text/javascript;base64,${Buffer.from(source).toString('base64')}`
);

const LABELS = { tokens: 'tok', runs_title: 'Runs', no_runs: 'No runs', cancel: 'Cancel' };

function mount(level = 'tree', labels = LABELS) {
  const el = new TfAgentActivity();
  el.labels = labels;
  el.setAttribute('level', level);
  document.body.appendChild(el);
  return el;
}

function rows(el) {
  return [...el.querySelectorAll('.tf-aa-run')].map((r) => ({
    agent: r.querySelector('.tf-aa-run-agent')?.textContent.trim(),
    meta: r.querySelector('.tf-aa-run-meta')?.textContent.trim(),
    title: r.querySelector('.tf-aa-run-main')?.getAttribute('title'),
  }));
}

// ---------------------------------------------------------------------------
// Hydration
// ---------------------------------------------------------------------------

test('a finished run is measured between its own two timestamps', () => {
  const el = mount();
  el.setRunInfo('r-1', {
    agent: 'root run #1',
    status: 'completed',
    startedAt: Date.UTC(2026, 7, 15, 14, 20, 57),
    finishedAt: Date.UTC(2026, 7, 15, 14, 22, 11),
    promptTokens: 6800,
    completionTokens: 634,
  });
  assert.deepEqual(rows(el), [{ agent: 'root run #1', meta: '1m 14s · 7434 tok', title: null }]);
});

test('the persisted start replaces the moment the client first looked', () => {
  const el = mount();
  // A replayed event creates the row with "now" as its start…
  el.applyEvent({ kind: 'child_spawned', run_id: 'r-2', agent: 'root' });
  assert.match(rows(el)[0].meta, /^0s · 0 tok$/);
  // …and the row it belongs to corrects it.
  el.setRunInfo('r-2', {
    status: 'running',
    startedAt: Date.now() - 95_000,
    promptTokens: 1000,
    completionTokens: 200,
  });
  const meta = rows(el)[0].meta;
  assert.match(meta, /^1m 3[45]s · 1200 tok$/, meta);
});

test('a run still going is measured against the clock, not left at zero', () => {
  const el = mount();
  el.setRunInfo('r-3', { status: 'running', startedAt: Date.now() - 42_000 });
  assert.match(rows(el)[0].meta, /^4[12]s · 0 tok$/);
});

test('a bare status update never invents a run the tree would count', () => {
  const el = mount();
  el.setRunStatus('ghost', 'completed');
  assert.equal(el.runCount, 0);
  assert.deepEqual(rows(el), []);
  // The same call on a row that exists still moves its status.
  el.setRunInfo('r-4', { status: 'running', startedAt: Date.now() });
  el.setRunStatus('r-4', 'failed');
  assert.equal(el.querySelector('tf-chip')?.textContent.trim(), 'failed');
});

test('an absent counter leaves the row at zero instead of guessing', () => {
  const el = mount();
  el.setRunInfo('r-5', { status: 'completed', startedAt: 1000, finishedAt: 4000 });
  assert.equal(rows(el)[0].meta, '3s · 0 tok');
});

test('the model the run addressed is offered without crowding the row', () => {
  const el = mount();
  el.setRunInfo('r-6', { status: 'completed', startedAt: 0, finishedAt: 1000, model: 'qwen3.5-nvfp4' });
  assert.equal(rows(el)[0].title, 'qwen3.5-nvfp4');
  assert.doesNotMatch(rows(el)[0].meta, /qwen/);
});

test('a timestamp handed over as a string is parsed, not dropped', () => {
  const el = mount();
  el.setRunInfo('r-7', {
    status: 'completed',
    startedAt: '2026-08-15T14:20:57Z',
    finishedAt: '2026-08-15T14:22:11Z',
  });
  assert.equal(rows(el)[0].meta, '1m 14s · 0 tok');
});

test('the parent link hydrated from a row nests the child under it', () => {
  const el = mount();
  el.setRunInfo('parent', { status: 'running', startedAt: Date.now(), agent: 'root #1' });
  el.setRunInfo('child', { status: 'running', startedAt: Date.now(), agent: 'sub #2', parentRunId: 'parent' });
  const depths = [...el.querySelectorAll('.tf-aa-run')].map((r) => r.getAttribute('style'));
  assert.deepEqual(depths, ['--depth:0', '--depth:1']);
});

// ---------------------------------------------------------------------------
// The account a run used (C02)
// ---------------------------------------------------------------------------

// The host composes the label (engine plus the account screens' own word for
// the mode) and hands it over per run; the widget adds no vocabulary of its own
// and, when the host says there is no account, shows no chip rather than an
// empty one.
function accountChips(el, scope) {
  return [...el.querySelectorAll(`${scope} tf-chip`)]
    .filter((chip) => chip.getAttribute('variant') === 'outline')
    .map((chip) => chip.textContent.trim());
}

const ACCOUNT_LABEL = 'Codex · konto globalne: Codex — firma';

test('a run row carries the account the host reported for it, and only for it', () => {
  const el = mount();
  el.setRunInfo('a-1', {
    agent: 'code-implementer', status: 'completed',
    startedAt: 1000, finishedAt: 2000, accountLabel: ACCOUNT_LABEL,
  });
  el.setRunInfo('a-2', { agent: 'code-reviewer', status: 'completed', startedAt: 1000, finishedAt: 2000 });
  assert.deepEqual(accountChips(el, '.tf-aa-run[data-run="a-1"]'), [ACCOUNT_LABEL]);
  assert.deepEqual(accountChips(el, '.tf-aa-run[data-run="a-2"]'), [], 'a run with no account grew a chip');
});

test('the bar names the account of the run its line is about', () => {
  const el = mount('bar');
  el.setRunInfo('b-1', { agent: 'code-implementer', status: 'running', startedAt: Date.now() });
  assert.deepEqual(accountChips(el, '.tf-aa-bar'), [], 'no account yet');
  el.setRunInfo('b-1', { accountLabel: ACCOUNT_LABEL });
  assert.deepEqual(accountChips(el, '.tf-aa-bar'), [ACCOUNT_LABEL]);
  // '' is an answer, not a missing field: the host says this run has no account.
  el.setRunInfo('b-1', { accountLabel: '' });
  assert.deepEqual(accountChips(el, '.tf-aa-bar'), []);
});

// ---------------------------------------------------------------------------
// The list has to survive a session whose runs have all finished
// ---------------------------------------------------------------------------

test('a pinned list keeps showing runs after the last one ends', () => {
  const el = mount();
  el.setRunInfo('r-8', { status: 'completed', startedAt: 1000, finishedAt: 2000 });
  assert.equal(el.hasActivity(), false, 'nothing is in flight');
  assert.equal(el.querySelector('.tf-aa-run') != null, true, 'and the row is still on screen');
  assert.equal(el.runCount, 1, 'the badge counts exactly what the list holds');
});

// ---------------------------------------------------------------------------
// The word on the status chip
// ---------------------------------------------------------------------------

// The chip printed the wire status verbatim, so a console in any language showed
// "waiting_user". The widget is i18n-agnostic by design — the host hands it a
// `run_status` map — so the chain under test is the real one: a `session_runs`
// status, the shipped `activityLabels`, the shipped `code_studio.run_status.*`
// dictionary, and the chip's text. Both halves of that chain are cut out of the
// files that ship them (the module under test pulls the whole dashboard in, so
// it is never imported here).
const sessionSource = readFileSync(join(here, '..', 'modules', 'code-studio-session.js'), 'utf8');

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

function cutFn(src, name) {
  const start = src.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`no definition: ${name}`);
  return cutBalanced(src, start, '{', '}');
}

// `RUN_STATUSES` is a two-line `new Set([...])`, which only bracket balance reads.
function cutSet(src, name) {
  const start = src.indexOf(`const ${name} = `);
  if (start < 0) throw new Error(`no constant: ${name}`);
  const eq = start + `const ${name} = `.length;
  return `${src.slice(start, eq)}${cutBalanced(src, eq, '(', ')')};`;
}

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'].map((lang) => [
  lang,
  JSON.parse(readFileSync(join(here, '..', '..', 'i18n', `${lang}.json`), 'utf8')).code_studio,
]);

function lookup(dict, key) {
  return key.split('.').reduce((node, part) => (node == null ? node : node[part]), dict);
}

/// The label dict `activityLabels()` hands the widget, bound to one locale. A key
/// the module asks for and the locale does not have is a failure here, not a raw
/// key on the screen.
function hostLabels(dict) {
  const t = (key) => {
    const text = lookup(dict, key);
    assert.equal(typeof text, 'string', `missing translation: code_studio.${key}`);
    return text;
  };
  return new Function('t', `
    ${cutSet(sessionSource, 'RUN_STATUSES')}
    ${cutFn(sessionSource, 'runStatusLabel')}
    ${cutFn(sessionSource, 'activityLabels')}
    return activityLabels();
  `)(t);
}

function localeDict(lang) {
  return LOCALES.find(([code]) => code === lang)[1];
}

test('a run status is stated in the operator’s language, never as its wire id', () => {
  for (const [lang, dict] of LOCALES) {
    const labels = hostLabels(dict);
    for (const status of ['waiting_user', 'timed_out', 'cancelling', 'running', 'completed']) {
      const el = mount('tree', labels);
      el.setRunInfo('r-status', { agent: 'root #1', status, startedAt: 1000, finishedAt: 2000 });
      const expected = dict.run_status[status];
      assert.equal(el.querySelector('.tf-aa-run tf-chip')?.textContent.trim(), expected, `${lang} ${status}`);
      // A language that spells the state its own way is the proof no raw id is on
      // the row; English spells a few of them exactly as the wire does.
      if (expected !== status) {
        assert.ok(
          !el.innerHTML.includes(status),
          `${lang} ${status}: the wire id is still on the row (${el.innerHTML})`,
        );
      }
      // The detail panel states the same run the same way — it is the second
      // place this chip is built.
      el.querySelector('[data-action="open-run"]').click();
      assert.equal(
        el.querySelector('.tf-aa-panel-head tf-chip')?.textContent.trim(),
        expected,
        `${lang} ${status} in the detail panel`,
      );
      if (expected !== status) {
        assert.ok(
          !el.innerHTML.includes(status),
          `${lang} ${status}: the wire id is still in the detail panel (${el.innerHTML})`,
        );
      }
    }
  }
});

test('a status neither the host nor the widget has a word for prints as it arrived', () => {
  const el = mount('tree', hostLabels(localeDict('pl')));
  el.setRunInfo('r-unknown', { status: 'projection_pending', startedAt: 1000, finishedAt: 2000 });
  assert.equal(el.querySelector('.tf-aa-run tf-chip').textContent.trim(), 'projection_pending');
});

test('a finished child states its status in the operator’s language too', () => {
  const dict = localeDict('pl');
  const el = mount('tree', hostLabels(dict));
  el.applyEvent({ kind: 'child_spawned', run_id: 'c-1', agent: 'sub #1' });
  el.applyEvent({ kind: 'child_finished', run_id: 'c-1', agent: 'sub #1', status: 'cancelled' });
  el.querySelector('.tf-aa-run[data-run="c-1"] [data-action="open-run"]').click();
  const detail = el.querySelector('.tf-aa-detail').textContent;
  assert.ok(detail.includes(dict.run_status.cancelled), `the child status is not translated: ${detail}`);
  assert.ok(!detail.includes('cancelled'), `the wire id is still on the step: ${detail}`);
});

// ---------------------------------------------------------------------------
// A run that timed out has ended
// ---------------------------------------------------------------------------

// `timed_out` is the status `delegate_cli` writes into `session_runs` when a CLI
// turn hits its deadline (`delegate_cli.rs`). It is an END, and the same set
// decides four separate things, so a status missing from it is not cosmetic: the
// run counts as activity, the bar counts it as "in background", it can drive the
// collapsed line, and it is offered a Cancel button for work that already stopped.
test('a timed-out run is not activity and is offered no Cancel', () => {
  const el = mount();
  el.setRunInfo('t-1', { agent: 'root #1', status: 'timed_out', startedAt: 1000, finishedAt: 91_000 });
  assert.equal(el.hasActivity(), false, 'a run that already ended is still listed as active');
  assert.equal(el.runCount, 1, 'and its row is still on the surface');
  assert.equal(
    el.querySelector('.tf-aa-run[data-run="t-1"] [data-action="cancel-run"]'), null,
    'a run that already ended was offered Cancel',
  );
});

// The chip is the other half: a status the tone map does not carry falls back to
// the neutral `info` a `queued` run already wears, which would paint a timeout as
// "nothing has happened yet". It is `warn` — the tone `interrupted` takes.
test('a timed-out run does not wear the neutral tone', () => {
  const el = mount();
  el.setRunInfo('t-2', { agent: 'root #2', status: 'timed_out', startedAt: 1000, finishedAt: 2000 });
  const chip = el.querySelector('.tf-aa-run[data-run="t-2"] tf-chip');
  assert.notEqual(chip.getAttribute('status'), 'info', 'a timed-out chip wears the queued tone');
  assert.equal(chip.getAttribute('status'), 'warn');
});

test('a timed-out child is finished on every surface that reads the set', () => {
  const el = mount('bar');
  el.setRunInfo('p-1', { agent: 'root #1', status: 'running', startedAt: 1000 });
  el.setRunInfo('c-1', {
    agent: 'sub #1', status: 'timed_out', startedAt: 5000, parentRunId: 'p-1',
  });
  // The background badge and the line's driver both go through the set, and both
  // would read a finished child as work still going.
  assert.equal(el.querySelector('.tf-aa-badge'), null, 'a finished child is still counted in background');
  assert.equal(
    el.querySelector('.tf-aa-line').textContent.trim(), 'root #1 · idle',
    'a finished run drove the collapsed line',
  );

  // The lifecycle step reporting the same timeout must not be painted the `ok` a
  // child that finished its work takes.
  const step = mount();
  step.applyEvent({ kind: 'child_finished', run_id: 'c-2', status: 'timed_out' });
  step.querySelector('.tf-aa-run[data-run="c-2"] [data-action="open-run"]').click();
  const dot = step.querySelector('.tf-aa-detail .tf-aa-step-dot');
  assert.ok(
    dot.className.includes('tone-warn'),
    `a timed-out child was painted as a success: ${step.querySelector('.tf-aa-detail').innerHTML}`,
  );
});

// `cancelling` is deliberately NOT in that set, and it must not drift in: the
// request has been made, the turn has not stopped. It stays in flight and wears
// the active tone, not the neutral one a `queued` or `cancelled` run wears.
test('a cancelling run is still in flight, not finished', () => {
  const el = mount();
  el.setRunInfo('x-1', { agent: 'root #3', status: 'cancelling', startedAt: 1000 });
  assert.equal(el.hasActivity(), true, 'a cancellation request ended the run early');
  const chip = el.querySelector('.tf-aa-run[data-run="x-1"] tf-chip');
  assert.notEqual(chip.getAttribute('status'), 'info', 'a cancelling chip wears the neutral tone');
  assert.equal(chip.getAttribute('status'), 'accent');
});

