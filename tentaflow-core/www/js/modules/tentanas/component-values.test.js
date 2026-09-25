// =============================================================================
// File: modules/tentanas/component-values.test.js
// Description: Every literal value a TentaNas screen writes into a tf-*
//       component attribute that the component checks against an allowlist
//       is one the component accepts (round-2 "M12-like" sweep). A value
//       outside the component's Set renders as its neutral default with no
//       error anywhere — a warning chip that silently reads as info, a
//       progress bar that loses its tone — so only a scan like this sees it.
//
//       The allowlists are read from the components' own source, so a
//       component that changes its Set changes what this test holds the
//       screens to. Values built at runtime (`status="${tone}"`) are not
//       literals and are out of this scan's reach; the screens route those
//       through helpers (`stateTone`, `jobTone`, `healthClass`) whose outputs
//       have their own tests.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const COMPONENTS = join(HERE, '..', '..', 'components');

// The Set literal named `name` in a component's source, as a Set of strings.
function componentSet(file, name) {
  const source = readFileSync(join(COMPONENTS, file), 'utf8');
  const match = new RegExp(`const ${name} = new Set\\(\\[([\\s\\S]*?)\\]\\)`).exec(source);
  assert.ok(match, `${file} defines ${name}`);
  return new Set([...match[1].matchAll(/'([^']*)'/g)].map((m) => m[1]));
}

// tag -> attribute -> the component's allowlist for it.
const ALLOWED = {
  'tf-chip': { status: componentSet('tf-chip.js', 'STATUS_CLASSES'), 'dot-tone': componentSet('tf-chip.js', 'DOT_TONES') },
  'tf-alert': { tone: componentSet('tf-alert.js', 'VALID_TONES') },
  'tf-progress-bar': { tone: componentSet('tf-progress-bar.js', 'VALID_TONES'), size: componentSet('tf-progress-bar.js', 'VALID_SIZES') },
  'tf-badge': { tone: componentSet('tf-badge.js', 'VALID_TONES') },
  'tf-stat-card': {
    accent: componentSet('tf-stat-card.js', 'ACCENT_CLASSES'),
    'delta-type': componentSet('tf-stat-card.js', 'DELTA_TYPES'),
    size: componentSet('tf-stat-card.js', 'SIZE_CLASSES'),
  },
  'tf-tabs': { tone: componentSet('tf-tabs.js', 'TAB_TONES') },
};

const SCREENS = [
  join(HERE, '..', 'tentanas.js'),
  ...readdirSync(HERE).filter((f) => f.endsWith('.js') && !f.endsWith('.test.js') && !f.startsWith('_')).map((f) => join(HERE, f)),
];

// Every `<tf-x … attr="literal" …>` in a screen's markup. A value holding
// `${` is built at runtime and skipped.
function literalAttributes(source) {
  const out = [];
  for (const tag of source.matchAll(/<(tf-[a-z-]+)\b([^<>]*)>/g)) {
    for (const attr of tag[2].matchAll(/\s([a-z-]+)="([^"]*)"/g)) {
      if (attr[2].includes('${')) continue;
      out.push({ tag: tag[1], name: attr[1], value: attr[2] });
    }
  }
  return out;
}

test('every literal tf-* attribute value on the TentaNas screens is one its component accepts', () => {
  let checked = 0;
  const wrong = [];
  for (const file of SCREENS) {
    for (const { tag, name, value } of literalAttributes(readFileSync(file, 'utf8'))) {
      const allowed = ALLOWED[tag]?.[name];
      if (!allowed) continue;
      checked += 1;
      if (!allowed.has(value)) wrong.push(`${file.split('/').pop()}: <${tag} ${name}="${value}">`);
    }
  }
  assert.ok(checked > 50, `the scan found the screens' markup (${checked} values)`);
  assert.deepEqual(wrong, []);
});

test('the sweep itself catches a value outside the allowlist', () => {
  const found = literalAttributes('<tf-chip status="warning" dot></tf-chip><tf-chip status="${tone}"></tf-chip>');
  assert.deepEqual(found, [{ tag: 'tf-chip', name: 'status', value: 'warning' }], 'a runtime value is not a literal');
  assert.equal(ALLOWED['tf-chip'].status.has('warning'), false, 'and "warning" is not a chip status');
});
