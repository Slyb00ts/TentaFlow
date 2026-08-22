// =============================================================================
// File: modules/agents.tool-picker.test.js
// Description: What the tool picker of the agent wizard promises about an addon
//       group. The regression pinned here: the group head used to be nothing but
//       the raw instance id (`deep-research-043b6b64`), so an admin granting an
//       agent access to an addon could not tell what the addon was or did. The
//       head now carries the addon's display name and its manifest one-liner,
//       and keeps the raw id as secondary text — two instances of the same
//       package share a name and only the id separates them.
//       agents.js imports the whole dashboard, so the functions under test are
//       cut out of the real file by brace matching and evaluated against stubs —
//       the code tested here is the code that ships.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'agents.js'), 'utf8');
const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const catalogs = Object.fromEntries(
  LOCALES.map((loc) => [
    loc,
    JSON.parse(readFileSync(join(here, `../../i18n/${loc}.json`), 'utf8')),
  ]),
);

function cut(src, name) {
  const start = src.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`no definition: ${name}`);
  let depth = 0;
  let i = src.indexOf('{', start);
  for (; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

const NAMES = ['addonGroupLabel', 'renderToolPicker'];

const PRELUDE = `
  const escapeHtml = (v) => String(v ?? '').replace(/&/g, '&amp;').replace(/</g, '&lt;');
  const escapeAttr = (v) => String(v ?? '').replace(/&/g, '&amp;').replace(/"/g, '&quot;');
  const sprite = (id) => '<svg data-icon="' + id + '"></svg>';
`;

/** A fresh module instance: `state.wizard` is module state the picker reads. */
function build(env) {
  const body = NAMES.map((n) => cut(source, n)).join('\n');
  // eslint-disable-next-line no-new-func
  const factory = new Function(
    't', 'state',
    `${PRELUDE}
     ${body}
     return { ${NAMES.join(', ')} };`,
  );
  return factory(env.t, env.state);
}

function lookup(root, path) {
  return path.split('.').reduce((acc, part) => (acc == null ? null : acc[part]), root) ?? null;
}

/** The real catalog answers every t() call, so an untranslated key shows up raw. */
function translator(locale) {
  return (key, vars) => {
    const value = lookup(catalogs[locale], `agents.${key}`);
    if (value === null) return `agents.${key}`;
    if (!vars) return value;
    return Object.entries(vars).reduce(
      (acc, [k, v]) => acc.split(`{${k}}`).join(String(v)),
      value,
    );
  };
}

/** Minimal picker host: the module only ever sets innerHTML on it. */
function makeEnv({ addons = [], core = [], selected = [] } = {}, locale = 'pl') {
  const host = { innerHTML: '', dataset: {}, addEventListener() {} };
  return {
    host,
    t: translator(locale),
    state: {
      wizard: {
        catalog: { addons, core },
        selectedTools: new Set(selected),
        body: { querySelector: (sel) => (sel === '#agent-wz-tools' ? host : null) },
      },
    },
  };
}

const GROUP = {
  addon_id: 'deep-research-043b6b64',
  display_name: 'Deep Research',
  description: 'Wielokrokowe badanie publicznego internetu.',
  tools: [{ name: 'deep-research-043b6b64.research', description: 'Zbadaj temat', parameters: {} }],
};

test('group head shows the addon name and description, id stays secondary', () => {
  const env = makeEnv({ addons: [GROUP] });
  const mod = build(env);
  mod.renderToolPicker();
  const html = env.host.innerHTML;

  assert.match(html, /<tf-chip status="accent">Deep Research<\/tf-chip>/);
  assert.match(html, /agents-tool-group-sub[^>]*>Wielokrokowe badanie publicznego internetu\./);
  // The raw id must remain readable — it is the only thing separating two
  // instances of the same package.
  assert.match(html, /class="agents-tool-group-id"[^>]*>deep-research-043b6b64</);
  assert.match(html, /title="Identyfikator instancji: deep-research-043b6b64"/);
  // The wildcard toggle still targets the whole addon.
  assert.match(html, /data-group-toggle="deep-research-043b6b64\.\*"/);
});

test('an addon without a name or description degrades to id + explicit fallback', () => {
  const env = makeEnv({ addons: [{ ...GROUP, display_name: '   ', description: '' }] });
  const mod = build(env);
  const { title, subtitle } = mod.addonGroupLabel({ ...GROUP, display_name: '  ', description: '' });

  assert.equal(title, 'deep-research-043b6b64');
  assert.equal(subtitle, catalogs.pl.agents.tools_group_no_description);

  mod.renderToolPicker();
  // No empty head and no raw i18n path leaking into the UI.
  assert.match(env.host.innerHTML, /<tf-chip status="accent">deep-research-043b6b64<\/tf-chip>/);
  assert.doesNotMatch(env.host.innerHTML, /agents\.tools_group_/);
});

test('a catalog missing the new fields renders the id, never "undefined"', () => {
  const env = makeEnv({ addons: [{ addon_id: 'memory-1', tools: [] }] });
  const mod = build(env);
  mod.renderToolPicker();

  assert.match(env.host.innerHTML, /<tf-chip status="accent">memory-1<\/tf-chip>/);
  assert.doesNotMatch(env.host.innerHTML, /undefined/);
});

test('both new keys exist in every locale and interpolate the id', () => {
  for (const locale of LOCALES) {
    const t = translator(locale);
    assert.notEqual(t('tools_group_no_description'), 'agents.tools_group_no_description', locale);
    const withId = t('tools_group_instance_id', { id: 'memory-1' });
    assert.notEqual(withId, 'agents.tools_group_instance_id', locale);
    assert.match(withId, /memory-1/, locale);
  }
});
