// ===== File: events-origins.test.js — the events screen offers exactly the origins the server knows =====

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = (path) => readFileSync(new URL(path, import.meta.url), 'utf8');

const rustSlugs = [...read('../../../src/flow_engine/dispatcher.rs')
  .match(/pub fn as_str\(self\) -> &'static str \{[\s\S]*?\n    \}/)[0]
  .matchAll(/FlowOrigin::\w+ => "(\w+)"/g)].map((match) => match[1]);
const source = read('./events.js');
const uiOrigins = [...source.match(/const ORIGINS = \[([\s\S]*?)\];/)[1].matchAll(/'(\w+)'/g)].map((m) => m[1]);
const classes = Object.fromEntries([...source.match(/const ORIGIN_CLASS = \{([\s\S]*?)\};/)[1]
  .matchAll(/(\w+): '([\w-]+)'/g)].map((m) => [m[1], m[2]]));

test('every FlowOrigin slug is a filter chip with a colour class', () => {
  assert.ok(rustSlugs.includes('process') && rustSlugs.includes('bus'));
  assert.deepEqual([...uiOrigins].sort(), [...rustSlugs].sort());
  const css = read('../../css/events.css');
  for (const slug of rustSlugs) {
    assert.ok(classes[slug], `${slug} has no class`);
    assert.ok(css.includes(`.ev-shell .${classes[slug]} {`), `${slug} class ${classes[slug]} is not styled`);
  }
});

test('every origin label exists in all five locales', () => {
  for (const language of ['pl', 'en', 'de', 'es', 'fr']) {
    const events = JSON.parse(read(`../../i18n/${language}.json`)).events;
    for (const slug of rustSlugs) assert.equal(typeof events[`origin_${slug}`], 'string', `${language}: origin_${slug}`);
  }
});
