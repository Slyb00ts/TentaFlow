import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const LANGS = ['pl', 'en', 'de', 'es', 'fr'];
const MODULES = fileURLToPath(new URL('..', import.meta.url));
const read = (path) => readFileSync(path, 'utf8');
const load = (l) => JSON.parse(read(fileURLToPath(new URL(`../../../i18n/${l}.json`, import.meta.url))));
const flat = (o, p = '') => Object.entries(o).flatMap(([k, v]) => (v && typeof v === 'object' ? flat(v, `${p}${k}.`) : [[`${p}${k}`, v]]));
const limitKeys = (pack) => flat(pack.services_edit, 'services_edit.').filter(([key]) => key.startsWith('services_edit.limits_'));
const slaOf = (pack) => new Map(flat(pack.sla, 'sla.').concat(flat(pack.tentabus.consumer.sla, 'tentabus.consumer.sla.'), limitKeys(pack)));
const vars = (s) => [...String(s).matchAll(/\{(\w+)\}/g)].map((x) => x[1]).sort().join(',');

/** The comparison the screen relies on: same keys, same placeholders, right plural forms, nothing empty. */
export function checkParity(packs) {
  const problems = [];
  const base = slaOf(packs.pl);
  for (const lang of LANGS) {
    const own = slaOf(packs[lang]);
    for (const key of base.keys()) if (!own.has(key)) problems.push(`${lang} lacks ${key}`);
    for (const key of own.keys()) if (!base.has(key)) problems.push(`${lang} has extra ${key}`);
    for (const [key, value] of own) {
      if (!String(value).trim()) problems.push(`${lang} ${key} is empty`);
      if (base.has(key) && vars(value) !== vars(base.get(key))) problems.push(`${lang} ${key} placeholders`);
      for (const m of String(value).matchAll(/\{\w+((?:\|[^}|]*)+)\}/g)) {
        const forms = m[1].split('|').length - 1;
        if (forms !== (lang === 'pl' ? 3 : 2)) problems.push(`${lang} ${key} plural forms`);
      }
    }
  }
  return problems;
}

const packs = Object.fromEntries(LANGS.map((l) => [l, load(l)]));

test('the SLA texts have the same keys, placeholders and plural forms in all five languages', () => {
  assert.ok(slaOf(packs.pl).size > 150);
  assert.deepEqual(checkParity(packs), []);
});

test('a key missing from one language fails the real checker', () => {
  const broken = structuredClone(packs);
  delete broken.de.sla.editor.field.name;
  delete broken.en.tentabus.consumer.sla.kpi_label;
  const problems = checkParity(broken);
  assert.ok(problems.includes('de lacks sla.editor.field.name'));
  assert.ok(problems.includes('en lacks tentabus.consumer.sla.kpi_label'));
  const changed = structuredClone(packs);
  changed.fr.sla.calls_per = '{limit} par';
  assert.ok(checkParity(changed).some((p) => p.includes('fr sla.calls_per placeholders')));
  const plural = structuredClone(packs);
  plural.pl.sla.services_count = '{count} {n|przepływ|przepływów}';
  assert.ok(checkParity(plural).some((p) => p.includes('plural forms')));
});

// Keys built from a template literal: every value the code can put in the hole is listed here.
const WINDOWS = [1, 60, 3600, 86400];
const TEMPLATES = {
  'window_${secs}': WINDOWS.map((w) => `window_${w}`),
  'window_short_${secs}': WINDOWS.map((w) => `window_short_${w}`),
  'window_per_${secs}': WINDOWS.map((w) => `window_per_${w}`),
  'state.${state}': ['ok', 'warn', 'breach', 'throttled'].map((x) => `state.${x}`),
  'breach.${breach.kind}': ['latency', 'errors', 'calls_warn', 'calls_limit', 'concurrency_saturated', 'timeouts'].map((x) => `breach.${x}`),
  'editor.hint.${wire}': ['max_concurrency', 'max_queue', 'queue_timeout_ms', 'call_limit'].map((x) => `editor.hint.${x}`),
  'error.${protocolErrorKind(err)}': ['denied', 'auth', 'not_found', 'node_unreachable', 'rate_limited', 'timeout', 'generic'].map((x) => `error.${x}`),
  'editor.field.${wire}': [
    'name', 'description', 'max_concurrency', 'max_queue', 'queue_timeout_ms', 'call_limit', 'call_window',
    'warn_at_pct', 'timeout_ms', 'latency_p95_ms', 'error_rate_pct', 'ordering',
  ].map((x) => `editor.field.${x}`),
  'consumer.sla.${thing}': [],
};

/** Every T()/slaT() call of `source`: the literals inside its argument list. */
function literalsOfCalls(source, callers) {
  const out = [];
  if (!callers.length) return out;
  const pattern = new RegExp(`(?<![\\w.])(${callers.join('|')})\\(`, 'g');
  for (const m of source.matchAll(pattern)) {
    let depth = 1;
    let i = m.index + m[0].length;
    const start = i;
    for (; i < source.length && depth > 0; i += 1) {
      if (source[i] === '(') depth += 1;
      else if (source[i] === ')') depth -= 1;
    }
    const args = source.slice(start, i - 1);
    for (const lit of args.matchAll(/'([^'\n]*)'|`([^`\n]*)`/g)) {
      if (/[!=]==?\s*$/.test(args.slice(0, lit.index))) continue; // a comparison operand, not a key
      out.push({ caller: m[1], text: lit[1] ?? lit[2], template: lit[2] != null });
    }
  }
  return out;
}

const keyLike = /^[a-z][a-z0-9_]*(\.[a-z0-9_${}()?.\s]+)*$/i;

test('every key the SLA screens use exists in all languages, templates included', () => {
  const known = (lang) => slaOf(packs[lang]);
  const files = [
    ...readdirSync(`${MODULES}sla`).filter((f) => f.endsWith('.js') && !f.endsWith('.test.js')).map((f) => ({ path: `${MODULES}sla/${f}`, ns: 'sla.', callers: ['T'] })),
    { path: `${MODULES}flows.js`, ns: 'sla.', callers: ['slaT'], explicit: /I18n\.t\(\s*'(sla\.[a-z0-9_.]+)'/g },
    { path: `${MODULES}services-limits.js`, ns: 'services_edit.', callers: ['tk'] },
    { path: `${MODULES}services-edit.js`, ns: '', callers: [], explicit: /I18n\.t\(\s*'(services_edit\.limits_[a-z_]+)'/g },
    { path: `${MODULES}tentabus/consumer-sla.js`, ns: 'sla.', callers: ['slaT'], tb: true },
    { path: `${MODULES}tentabus/consumer-detail.js`, ns: '', callers: [], tb: true },
  ];
  const missing = [];
  const unlisted = [];
  const used = new Set();
  const want = (lang, key, where) => { if (!known(lang).has(key)) missing.push(`${lang} ${key} (${where})`); };
  for (const file of files) {
    const source = read(file.path);
    const where = file.path.slice(MODULES.length);
    const calls = literalsOfCalls(source, file.callers);
    if (file.tb) calls.push(...literalsOfCalls(source, ['T']).map((c) => ({ ...c, caller: 'tbT' })));
    for (const call of calls) {
      const isTb = call.caller === 'tbT';
      if (isTb && !call.text.startsWith('consumer.sla.')) continue;
      if (!isTb && !keyLike.test(call.text)) continue;
      const prefix = isTb ? 'tentabus.' : file.ns;
      const keys = call.template ? TEMPLATES[call.text] : [call.text];
      if (!keys) { unlisted.push(`${where}: \`${call.text}\``); continue; }
      for (const key of keys) {
        used.add(prefix + key);
        for (const lang of LANGS) want(lang, prefix + key, where);
      }
    }
    for (const m of source.matchAll(file.explicit ?? /$^/g)) { used.add(m[1]); for (const lang of LANGS) want(lang, m[1], where); }
  }
  assert.deepEqual(unlisted, [], 'a template key that the test does not enumerate');
  assert.deepEqual(missing, []);
  assert.ok(used.size > 120, `only ${used.size} keys were checked`);
});
