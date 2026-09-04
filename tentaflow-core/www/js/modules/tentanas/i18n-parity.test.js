// =============================================================================
// File: modules/tentanas/i18n-parity.test.js
// Description: The WHOLE `tentanas` namespace must have an identical key set
// across the five locales and interpolation placeholders matching the Polish
// source, so a missing translation never surfaces as a raw key. The single
// keys the screen owns outside that namespace (the uninstall entries) are
// checked one by one — and no locale may carry a value with its diacritics
// stripped, which is what a key set comparison alone cannot see.
// =============================================================================

import { WWW_ROOT } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const NAMESPACE = 'tentanas';
// Every `kind` the Rust side emits from `tentanas/mod.rs`, so a teardown row
// can never fall back to its English description in a localized dialog.
const SINGLE_KEYS = [
  'addon_uninstall.entries.tentanas_ksmbd_config',
  'addon_uninstall.entries.tentanas_smb_config',
  'addon_uninstall.entries.tentanas_nfs_exports',
  'addon_uninstall.entries.tentanas_audit_rules',
  'addon_uninstall.entries.tentanas_iscsi_targets',
  'addon_uninstall.entries.tentanas_nvmet_targets',
  'addon_uninstall.entries.tentanas_nfs_conf',
  'addon_uninstall.entries.tentanas_arc_limit',
  'addon_uninstall.entries.tentanas_fleet_mounts',
  'addon_uninstall.entries.tentanas_pools',
  'addon_uninstall.entries.tentanas_config_backup',
  'addon_uninstall.entries.tentanas_data_dir',
  'addon_uninstall.entries.tentanas_keystore',
  'addon_uninstall.entries.tentanas_helper',
  'addon_uninstall.entries.tentanas_sudoers',
];

const bundles = Object.fromEntries(LOCALES.map((l) => [l, JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${l}.json`), 'utf8'))]));
const dig = (obj, path) => path.split('.').reduce((o, k) => (o && typeof o === 'object' ? o[k] : undefined), obj);
const flatten = (obj, prefix = '') => Object.entries(obj).flatMap(([k, v]) => (v && typeof v === 'object' ? flatten(v, `${prefix}${k}.`) : [`${prefix}${k}`]));
// `{n}` style placeholders and the `{n|one|few|many}` plural selector both name the parameter first.
const placeholders = (s) => [...String(s).matchAll(/\{([a-zA-Z0-9_]+)(?:\|[^}]*)?\}/g)].map((m) => m[1]).sort();

const reference = flatten(dig(bundles.pl, NAMESPACE)).sort();

test('the whole tentanas namespace has the same key set in all five locales', () => {
  assert.ok(reference.length > 0, 'the pl namespace is not empty');
  for (const l of LOCALES) {
    const keys = flatten(dig(bundles[l], NAMESPACE) || {}).sort();
    assert.deepEqual(keys, reference, `${NAMESPACE} keys in ${l} match pl`);
  }
});

test('every tentanas value is a non-empty string in all five locales', () => {
  for (const key of reference) {
    for (const l of LOCALES) {
      const v = dig(bundles[l], `${NAMESPACE}.${key}`);
      assert.equal(typeof v, 'string', `${NAMESPACE}.${key} in ${l}`);
      assert.ok(v.trim().length > 0, `${NAMESPACE}.${key} in ${l} is not blank`);
    }
  }
});

test('the single keys added around the namespace exist everywhere and are non-empty strings', () => {
  for (const key of SINGLE_KEYS) {
    for (const l of LOCALES) {
      const v = dig(bundles[l], key);
      assert.equal(typeof v, 'string', `${key} in ${l}`);
      assert.ok(v.trim().length > 0, `${key} in ${l} is not blank`);
    }
  }
});

// Words that ALWAYS carry a diacritic in their language, listed in the ASCII
// form somebody types when they translate without the keyboard for it.
//
// WHY this test exists: the key-set comparison above is what let 155 target
// strings ship in `es.json` and `fr.json` with zero accented characters — and
// 16 German ones with `oe`/`ue`/`ss` — while the rest of those same files were
// properly written. A missing accent is not a typo in a UI: it is a different
// register, and half a file in it reads as machine output.
//
// It cannot be a general rule (there is no way to tell "this ASCII string is
// wrong" from the outside), and it deliberately holds only words whose
// accented form is the ONLY correct one: `muss`, `esta` and `authentifie` are
// all real words and are not here. Placeholders are stripped first, because
// `{detail}` is a parameter name and not French. Extend the lists when a new
// slice trips over the same thing.
const DIACRITIC_TRAPS = {
  pl: ['sie', 'moze', 'wlacz', 'wylacz', 'polaczenie', 'sciezka', 'urzadzenie', 'bedzie', 'ktory', 'ktora', 'ktore', 'wiecej', 'blad', 'bledy', 'jesli', 'dzieki', 'zadne', 'zadnych', 'rowniez', 'czesc', 'pozniej'],
  de: ['loeschen', 'loescht', 'verfuegbar', 'groesse', 'groesser', 'fuer', 'waehle', 'koennen', 'muessen', 'schluessel', 'uebergang', 'zerstoeren', 'veroeffentlicht', 'kuerzere', 'laengere', 'haelt', 'laesst', 'faelschen', 'gewaehlte', 'pruefe', 'pruefen', 'aeltere', 'ueber', 'moeglich', 'naechste', 'zurueck', 'aendern', 'hinzufuegen', 'ausfuehren', 'unterstuetzt', 'schliessen', 'waehrend', 'gehoert', 'erhoeht'],
  es: ['accion', 'configuracion', 'autenticacion', 'conexion', 'exportacion', 'informacion', 'version', 'sesion', 'opcion', 'particion', 'replicacion', 'numero', 'linea', 'ningun', 'tambien', 'despues', 'aqui', 'asi', 'estan', 'anade', 'tamano', 'contrasena', 'maximo', 'minimo', 'ultimo', 'automatico', 'dias', 'rapido', 'publico', 'unico', 'creacion', 'eliminacion', 'activacion', 'desactivacion'],
  fr: ['controleur', 'controleurs', 'noeud', 'noeuds', 'systeme', 'systemes', 'meme', 'apres', 'cle', 'cles', 'hote', 'hotes', 'reseau', 'creer', 'donnees', 'verifiez', 'deja', 'arretee', 'arreter', 'connectes', 'immediatement', 'decision', 'deliberee', 'dedie', 'recapitulatif', 'prefere', 'autorises', 'declare', 'elements', 'requete', 'propriete', 'securite', 'activite', 'parametre', 'precedent', 'derniere', 'premiere', 'tres', 'operation', 'selectionne', 'termine', 'recuperer', 'integrite', 'protege'],
  en: [],
};

test('no locale carries a value with its diacritics stripped', () => {
  for (const [locale, words] of Object.entries(DIACRITIC_TRAPS)) {
    if (!words.length) continue;
    const trap = new RegExp(`(?<!\\p{L})(${words.join('|')})(?!\\p{L})`, 'iu');
    for (const key of reference) {
      const value = String(dig(bundles[locale], `${NAMESPACE}.${key}`)).replace(/\{[^}]*\}/g, ' ');
      const hit = trap.exec(value);
      assert.equal(hit, null, `${NAMESPACE}.${key} in ${locale} spells "${hit && hit[1]}" without its diacritics: ${value}`);
    }
    for (const key of SINGLE_KEYS) {
      const value = String(dig(bundles[locale], key)).replace(/\{[^}]*\}/g, ' ');
      const hit = trap.exec(value);
      assert.equal(hit, null, `${key} in ${locale} spells "${hit && hit[1]}" without its diacritics: ${value}`);
    }
  }
});

test('interpolation placeholders match the Polish source in every locale', () => {
  for (const key of reference) {
    const full = `${NAMESPACE}.${key}`;
    const expected = placeholders(dig(bundles.pl, full));
    for (const l of LOCALES) {
      assert.deepEqual(placeholders(dig(bundles[l], full)), expected, `${full} placeholders in ${l}`);
    }
  }
  for (const key of SINGLE_KEYS) {
    const expected = placeholders(dig(bundles.pl, key));
    for (const l of LOCALES) assert.deepEqual(placeholders(dig(bundles[l], key)), expected, `${key} placeholders in ${l}`);
  }
});


test('every literal key the block modules ask for exists in all five locales', () => {
  // The parity test above compares the five FILES with each other, which says
  // nothing about the code: a key deleted from all five, or typed wrong in a
  // `T(...)` call, renders as the raw string `tentanas.targets.whatever` on
  // screen and no test anywhere fails. This closes that from the other side —
  // the JS asks, the bundles answer.
  //
  // Only LITERAL keys are checked. A handful of call sites build the key from
  // a variable (`transport_${t}`, `AUTH_LABEL_KEY[...]`, the alert subject
  // kinds), and a scanner that tried to resolve those would either miss them
  // or invent them; those are covered by the tests that render the components.
  const dir = join(WWW_ROOT, 'js/modules/tentanas');
  const files = readdirSync(dir).filter((f) => f.endsWith('.js') && !f.endsWith('.test.js'));
  const missing = [];
  for (const file of files) {
    const source = readFileSync(join(dir, file), 'utf8');
    for (const m of source.matchAll(/\bT\('([a-z0-9_.]+)'/g)) {
      // A literal ending in `_` or `.` is the first half of a key built by
      // concatenation (`T('jobs.status_' + status)`). Resolving those would
      // mean guessing the other half; the tests that render those components
      // are what cover them.
      if (/[._]$/.test(m[1])) continue;
      // `T` is the namespaced helper (`format.js`: `I18n.t('tentanas.' + k)`),
      // so what the code writes is a key RELATIVE to the namespace.
      const key = `${NAMESPACE}.${m[1]}`;
      for (const locale of LOCALES) {
        if (dig(bundles[locale], key) === undefined) missing.push(`${locale}: ${key} (${file})`);
      }
    }
  }
  assert.deepEqual(missing, [], 'keys the code asks for and no bundle has');
});

test('no locale key of this namespace is dead: something in the JS asks for it', () => {
  // The other direction, and the one that let `wizard_target.portal_option`
  // sit in five files for two rounds with nothing calling it. Scoped to the
  // two wizard/table modules' own namespaces so it cannot fail on keys that
  // belong to the Rust side (feature ids, job kinds, alert subject kinds) or
  // to a screen this slice does not own.
  const dir = join(WWW_ROOT, 'js/modules/tentanas');
  const sources = readdirSync(dir)
    .filter((f) => f.endsWith('.js'))
    .map((f) => readFileSync(join(dir, f), 'utf8'))
    .join('\n')
    + readFileSync(join(WWW_ROOT, 'js/modules/tentanas.js'), 'utf8');
  const dead = [];
  // `targets` was NOT scanned, so a dead `tentanas.targets.*` key passed —
  // and that section is the bigger of the two.
  for (const section of ['wizard_target', 'targets']) {
    for (const key of Object.keys(bundles.pl[NAMESPACE][section])) {
      // The fully qualified key, or an interpolated tail: `T(\`${section}.${…}\`)`
      // and `T(key)` where `key` was chosen a line earlier. The tail form is
      // matched as `'<key>'` INSIDE a `T(` or a `${` — a bare substring match
      // let a short name like `title` be satisfied by any unrelated string
      // literal in twenty scanned files.
      const qualified = sources.includes(`${section}.${key}`);
      const interpolated = new RegExp(`(?:T\\(|\\$\\{)[^\n]{0,120}'${key.replace(/[.*+?^$()|[\]\\]/g, '\\$&')}'`).test(sources);
      if (!qualified && !interpolated) dead.push(`${section}.${key}`);
    }
  }
  assert.deepEqual(dead, [], 'keys five locales carry and nothing renders');
});
