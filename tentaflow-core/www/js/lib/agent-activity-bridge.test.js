// =============================================================================
// File: lib/agent-activity-bridge.test.js
// Description: Tests for the label dict `activityLabels()` hands
//       <tf-agent-activity>. The bridge serves THREE hosts — the agent
//       playground (`agents.js`), the Project Studio generation panel
//       (`project-studio.js`) and chat (`attachAgentActivity`) — and none of
//       them had a `run_status` map, so a run in `waiting_user` or `timed_out`
//       read in English on an otherwise Polish screen while the widget's own
//       default map said "waiting for user". These pin the real chain: the
//       shipped bridge dict, the shipped `code_studio.run_status.*` and
//       `agents.run_status_interrupted` dictionaries, the shipped widget, and
//       the word on the chip.
//
//       Both halves of that chain are cut out of the files that ship them: the
//       bridge imports `ApiBinary`/`I18n` by absolute URL (which Node cannot
//       resolve) and the widget is imported by the whole dashboard, so neither
//       module is imported here.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));

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

const bridgeSource = readFileSync(join(here, 'agent-activity-bridge.js'), 'utf8');

// The real `I18n.t` answers a key it cannot resolve with the key PATH, not with
// `undefined`: `lookup()` returns a string or null, and `t` falls back to the
// path (`i18n.js:131-135`). The stub reproduces that, so a key the bridge asks
// for that no locale has surfaces here as `code_studio.run_status.foo` — the
// literal text that would reach the screen — instead of being quietly covered by
// the Polish fallback strings beside each key (which `||` never reaches in
// production, for the same reason).
function stubI18n(dict) {
  return {
    t: (key) => {
      const value = key.split('.').reduce((node, part) => (
        node == null ? node : node[part]
      ), dict);
      return typeof value === 'string' ? value : key;
    },
  };
}

function bridgeLabels(dict) {
  return new Function('I18n', `
    ${cutFn(bridgeSource, 'activityLabels')}
    return activityLabels();
  `)(stubI18n(dict));
}

// The shipped `activityStatusText`, with the shipped `activityLabels()` in scope
// for its own default parameter — the two entry points a host calls are wired to
// each other exactly as they ship.
function bridgeStatus(dict) {
  return new Function('I18n', `
    ${cutFn(bridgeSource, 'activityLabels')}
    ${cutFn(bridgeSource, 'activityStatusText')}
    return activityStatusText;
  `)(stubI18n(dict));
}

const widgetSource = readFileSync(join(here, '..', 'components', 'tf-agent-activity.js'), 'utf8')
  .replace(/^import\s+'[^']*';$/gm, '');
const { TfAgentActivity } = await import(
  `data:text/javascript;base64,${Buffer.from(widgetSource).toString('base64')}`
);

// The widget's own English floor, cut from the source that ships it: a locale
// word identical to it is a status the locale left untranslated.
function cutConst(src, name) {
  const start = src.indexOf(`const ${name} = `);
  if (start < 0) throw new Error(`no constant: ${name}`);
  const eq = start + `const ${name} = `.length;
  return `${src.slice(start, eq)}${cutBalanced(src, eq, '{', '}')};`;
}

// `DEFAULT_LABELS` is built from `DEFAULT_RUN_STATUS`, so both travel together.
const ENGLISH_DEFAULTS = new Function(
  `${cutConst(widgetSource, 'DEFAULT_RUN_STATUS')} return DEFAULT_RUN_STATUS;`,
)();
const ENGLISH_LABELS = new Function(
  `${cutConst(widgetSource, 'DEFAULT_RUN_STATUS')}${cutConst(widgetSource, 'DEFAULT_LABELS')} return DEFAULT_LABELS;`,
)();

// The two run families the widget is pointed at. `session_runs` is the nine
// `code_studio.run_status.*` states (`cancelling`/`timed_out` are its own);
// `agent_runs` is the CHECK set that carries `interrupted`, worded by `agents.*`.
const SESSION_STATES = [
  'queued', 'running', 'waiting', 'waiting_user', 'completed', 'failed',
  'cancelled', 'cancelling', 'timed_out',
];
const AGENT_STATES = ['interrupted'];

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'].map((lang) => [
  lang,
  JSON.parse(readFileSync(join(here, '..', '..', 'i18n', `${lang}.json`), 'utf8')),
]);

/// The word the locale itself carries for a state, from the namespace that
/// family's runs are worded in.
function localeWord(dict, status) {
  const value = AGENT_STATES.includes(status)
    ? dict.agents?.run_status_interrupted
    : dict.code_studio?.run_status?.[status];
  assert.equal(typeof value, 'string', `the ${status} word is missing from the locale`);
  return value;
}

function chipText(el, runId) {
  return el.querySelector(`.tf-aa-run[data-run="${runId}"] tf-chip`)?.textContent.trim();
}

function mount(labels) {
  const el = new TfAgentActivity();
  el.labels = labels;
  el.setAttribute('level', 'tree');
  document.body.appendChild(el);
  return el;
}

// ---------------------------------------------------------------------------
// The dict the three hosts build from
// ---------------------------------------------------------------------------

test('the bridge words every run status either run family can report', () => {
  for (const [lang, dict] of LOCALES) {
    const labels = bridgeLabels(dict);
    assert.equal(typeof labels.run_status, 'object', `${lang}: no run_status map`);
    for (const status of [...SESSION_STATES, ...AGENT_STATES]) {
      const word = labels.run_status[status];
      assert.equal(word, localeWord(dict, status), `${lang} ${status}`);
      // A value that is still a dotted path is a key the locale never had —
      // every word has to be a translation, not a lookup that failed.
      assert.ok(
        !word.includes('run_status'),
        `${lang} ${status}: the map carries a key path, not a word (${word})`,
      );
      // English spells several states exactly as the wire does, so only a locale
      // that spells it differently proves the default was not what answered.
      if (lang !== 'en') {
        assert.notEqual(
          word,
          ENGLISH_DEFAULTS[status],
          `${lang} ${status}: still the widget's English default`,
        );
      }
    }
  }
});

// ---------------------------------------------------------------------------
// …and what the widget states on those screens
// ---------------------------------------------------------------------------

test('a bridge-hosted screen states a run status in the operator’s language', () => {
  for (const [lang, dict] of LOCALES) {
    const labels = bridgeLabels(dict);
    for (const status of [...SESSION_STATES, ...AGENT_STATES]) {
      const expected = localeWord(dict, status);
      const el = mount(labels);
      el.setRunInfo('r-status', { agent: 'root #1', status, startedAt: 1000, finishedAt: 2000 });
      assert.equal(chipText(el, 'r-status'), expected, `${lang} ${status}`);
      if (expected !== status) {
        assert.ok(
          !el.innerHTML.includes(status),
          `${lang} ${status}: the wire id is still on the row (${el.innerHTML})`,
        );
      }
    }
  }
});

// ---------------------------------------------------------------------------
// A host map is a layer over the widget's, not a replacement for it
// ---------------------------------------------------------------------------

test('a host map that words one run family keeps the widget’s word for the other', () => {
  // The bridge words the session family; a host that words only its own states
  // (`agents.js` has the agent_runs set) must not blank the rest of the map —
  // the widget documents exactly that fall-through.
  const el = mount({ run_status: { running: 'w toku' } });
  el.setRunInfo('r-1', { status: 'timed_out', startedAt: 1000, finishedAt: 2000 });
  assert.equal(chipText(el, 'r-1'), 'timed out', 'the widget lost its own word for the state');
  // A status in NEITHER map still prints exactly as it arrived.
  el.setRunInfo('r-2', { status: 'projection_pending', startedAt: 1000, finishedAt: 2000 });
  assert.equal(chipText(el, 'r-2'), 'projection_pending');
});

// ---------------------------------------------------------------------------
// A counted spawn states its number in the operator's language
// ---------------------------------------------------------------------------

const SPAWN_COUNT = 5;

test('a counted spawn states the count in the operator’s language', () => {
  for (const [lang, dict] of LOCALES) {
    const template = dict.agent_activity?.status_spawn_many;
    assert.equal(typeof template, 'string', `${lang}: agent_activity.status_spawn_many is missing`);
    const expected = template.replaceAll('{count}', String(SPAWN_COUNT));
    const status = bridgeStatus(dict)({ kind: 'child_spawned', count: SPAWN_COUNT, agent: 'child-1' });
    assert.equal(status, expected, lang);
    // The number is what the line exists to state, and the placeholder is the
    // only thing standing between the locale template and the operator: a line
    // that lost either is a line that no longer says how many.
    assert.ok(status.includes(String(SPAWN_COUNT)), `${lang}: the count is not stated (${status})`);
    assert.ok(!status.includes('{count}'), `${lang}: the placeholder reached the screen (${status})`);
    // English here is 'Spawning {count} agents', so a word equal to the widget's
    // floor is a line no locale answered.
    assert.notEqual(
      status,
      ENGLISH_LABELS.step_child_many.replaceAll('{count}', String(SPAWN_COUNT)),
      `${lang}: still the widget's English default`,
    );
  }
});

test('the widget’s spawn floor is itself a countable template', () => {
  // The floor is the widget's, and the widget substitutes nothing: a host that
  // reads it off `widget.labels` hands the template on as it stands, so it has
  // to carry the placeholder the substitution needs.
  assert.equal(typeof ENGLISH_LABELS.step_child_many, 'string');
  assert.ok(ENGLISH_LABELS.step_child_many.includes('{count}'));
});

test('a single spawn still names the child instead of counting it', () => {
  const [lang, dict] = LOCALES[0];
  const labels = bridgeLabels(dict);
  const status = bridgeStatus(dict);
  assert.equal(status({ kind: 'child_spawned', count: 1, agent: 'child-1' }, labels), `${labels.step_child} · child-1`, lang);
  // No count on the event at all is a spawn of one, not of NaN.
  assert.equal(status({ kind: 'child_spawned', agent: 'child-2' }, labels), `${labels.step_child} · child-2`, lang);
});

test('a dict without the spawn word states the count and never throws', () => {
  const [, dict] = LOCALES[0];
  const status = bridgeStatus(dict);
  const { step_child_many, ...withoutWord } = bridgeLabels(dict);

  // The realistic shape: a host dict built from the widget's own words, which
  // carries `step_child` and not the plural wording. The number must survive it.
  const text = status({ kind: 'child_spawned', count: 4, agent: 'child-1' }, withoutWord);
  assert.equal(text, `${withoutWord.step_child} · 4`);
  assert.ok(!text.includes('undefined'), `a missing word reached the screen (${text})`);
  assert.ok(!text.includes('{count}'), `the placeholder reached the screen (${text})`);

  // And with nothing at all: no key, no template, no `undefined.replace(...)`.
  assert.doesNotThrow(() => {
    assert.equal(status({ kind: 'child_spawned', count: 3, agent: 'child-1' }, {}), 'sub-agent · 3');
    // An empty or non-string word is a word the dict does not have.
    assert.equal(status({ kind: 'child_spawned', count: 3 }, { step_child_many: '' }), 'sub-agent · 3');
    assert.equal(status({ kind: 'child_spawned', count: 3 }, { step_child_many: null }), 'sub-agent · 3');
    assert.equal(status({ kind: 'child_spawned', count: 3 }, { step_child_many: 7 }), 'sub-agent · 3');
    // Inputs a stream can actually carry: no event, no count, a count that is
    // not a number, a count that is not above one. Those take the single-child
    // line, so only its contract is pinned here — a string, no `undefined`, no
    // placeholder — not the spacing that branch already had.
    assert.equal(status(null), '');
    for (const ev of [
      { kind: 'child_spawned' },
      { kind: 'child_spawned', count: 'many' },
      { kind: 'child_spawned', count: -2 },
      { kind: 'child_spawned', count: null },
    ]) {
      const line = status(ev, withoutWord);
      assert.equal(typeof line, 'string', JSON.stringify(ev));
      assert.ok(!line.includes('undefined'), `${JSON.stringify(ev)} → ${line}`);
      assert.ok(!line.includes('{count}'), `${JSON.stringify(ev)} → ${line}`);
    }
  });
});
