// =============================================================================
// File: modules/tentabus/topic-hiding-key.test.js
// Description: "Zmień klucz haszowania" (F4 B7): the window says what rotating
// the organization's hash key does before anyone confirms — the old hashes stop
// matching in EVERY instance of the organization, the figures are only what
// this node knows ("co najmniej") — and wants the words retyped; the request
// names the version the window showed; a refusal stays in the window; the note
// after the change names the new version and what it touched.
// =============================================================================

import { window, I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { reachText, keyImpact, rotatedNotice, openHashKeyRotate } = await import('./topic-hiding-key.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const tick = (ms = 20) => new Promise((r) => setTimeout(r, ms));
const closeAll = () => document.querySelectorAll('tf-window').forEach((w) => w.remove());

const AUG_12 = new Date(2026, 7, 12, 10, 0, 0).getTime();
const INFO = { exists: true, keyVersion: 3, createdAtMs: AUG_12 - 86_400_000, rotatedAtMs: AUG_12, hashingRules: 3, hashingTopics: 2, hashingInstances: 1 };

test('the reach is "at least" and from this node, in the right plural; none is said plainly', () => {
  assert.equal(reachText(INFO), 'Ten node widzi co najmniej 3 zasady „Zahaszuj” w 2 topikach (1 instancji).');
  assert.equal(reachText({ ...INFO, hashingRules: 1, hashingTopics: 1, hashingInstances: 1 }), 'Ten node widzi co najmniej 1 zasadę „Zahaszuj” w 1 topiku (1 instancji).');
  assert.equal(reachText({ ...INFO, hashingRules: 12, hashingTopics: 5, hashingInstances: 2 }), 'Ten node widzi co najmniej 12 zasad „Zahaszuj” w 5 topikach (2 instancjach).');
  assert.equal(reachText({ ...INFO, hashingRules: 0, hashingTopics: 0, hashingInstances: 0 }), 'Ten node nie widzi żadnej zasady „Zahaszuj”, ale mogą ją mieć instancje tej organizacji na innych nodach.');
  assert.equal(reachText(undefined), 'Ten node nie widzi żadnej zasady „Zahaszuj”, ale mogą ją mieć instancje tej organizacji na innych nodach.', 'nothing counted is not guessed as something');
});

test('the impact: the whole organization, the old hashes, the counts from this node and the version step', () => {
  assert.deepEqual(keyImpact({ info: INFO, orgName: 'Przychodnia Zdrowie' }), [
    'Klucz jest wspólny dla całej organizacji Przychodnia Zdrowie — zmiana dotyczy wszystkich jej instancji TentaBus, nie tylko tej.',
    'Hasze policzone dotychczasowym kluczem przestaną pasować do nowych — w każdej instancji organizacji.',
    'Ten node widzi co najmniej 3 zasady „Zahaszuj” w 2 topikach (1 instancji).',
    'To dane z tego noda: zasady instancji działających na innych nodach nie są tu policzone, więc dotkniętych zasad może być więcej.',
    'Obecny klucz ma wersję 3 (ustawiony 12.08.2026); nowy dostanie wersję 4.',
  ]);
  assert.match(keyImpact({ info: INFO, orgName: null })[0], /^Klucz jest wspólny dla całej organizacji — zmiana dotyczy wszystkich jej instancji/);
});

test('the note after the change names the new version and what the change touched', () => {
  const notice = rotatedNotice({ key: { ...INFO, keyVersion: 4 }, orgName: 'Przychodnia Zdrowie' });
  assert.equal(notice.title, 'Zmieniono klucz haszowania');
  assert.equal(notice.text, 'Klucz organizacji Przychodnia Zdrowie ma teraz nową wersję (4). Hasze policzone poprzednim kluczem nie pasują już do nowych w żadnej instancji tej organizacji. Ten node widzi co najmniej 3 zasady „Zahaszuj” w 2 topikach (1 instancji).');
  assert.match(rotatedNotice({ key: { ...INFO, keyVersion: 4 }, orgName: null }).text, /^Klucz organizacji ma teraz nową wersję \(4\)\./);
});

function open(overrides = {}) {
  const calls = { rotated: [], done: [] };
  const win = openHashKeyRotate({
    info: INFO,
    orgName: 'Przychodnia Zdrowie',
    rotate: async (version) => { calls.rotated.push(version); return { key: { ...INFO, keyVersion: version + 1 } }; },
    describeError: (err) => `błąd: ${err.message}`,
    onRotated: (key) => calls.done.push(key),
    ...overrides,
  });
  return { win, calls };
}

const retype = (win, value) => {
  const input = win.querySelector('#retype-input');
  input.value = value;
  input.dispatchEvent(new CustomEvent('input', { detail: { value } }));
};
const confirm = (win) => win.dispatchEvent(new CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));

test('the window says what changes, wants "zmień klucz" retyped, and sends the version it showed', async () => {
  closeAll();
  const { win, calls } = open();
  assert.equal(win.getAttribute('modal'), '');
  assert.equal(norm(win._titleEl.textContent), 'Zmień klucz haszowania');
  const text = norm(win.textContent);
  assert.match(text, /Po zmianie stare hasze przestaną pasować\. Ta sama wartość da inny hasz niż dotąd, więc programy, które łączą dane po haszu, nie połączą wiadomości sprzed zmiany z nowymi\./);
  assert.match(norm(win.querySelector('[data-role="explain"]').textContent), /^Klucz haszowania to tajny ciąg znaków, z którego liczy się hasz\..*Klucz jest wspólny dla całej organizacji Przychodnia Zdrowie\.$/, 'the window says what the key is before it says what changing it does');
  assert.match(text, /Co się stanie po zmianie:/);
  assert.equal(win.querySelectorAll('[data-role="impact"] li').length, 5);
  assert.match(text, /Ten node widzi co najmniej 3 zasady „Zahaszuj” w 2 topikach \(1 instancji\)\./);
  assert.match(text, /To dane z tego noda/);
  assert.match(text, /Zmiana klucza zapisze się w dzienniku audytu\./);
  assert.match(text, /Przepisz słowa, aby potwierdzić: zmień klucz/);
  const button = win.querySelector('[data-action="confirm"]');
  assert.match(button.textContent, /Zmień klucz/);
  assert.ok(button.hasAttribute('disabled'), 'nothing is rotated until the words are typed');
  retype(win, 'zmień');
  assert.ok(button.hasAttribute('disabled'));
  retype(win, 'zmień klucz');
  assert.equal(button.hasAttribute('disabled'), false);
  confirm(win);
  await tick();
  await tick();
  assert.deepEqual(calls.rotated, [3], 'the request names the version of the key the window showed');
  assert.deepEqual(calls.done.map((k) => k.keyVersion), [4], 'the shell gets the new key, with the version it has now');
  await tick(400);
  assert.equal(win.isConnected, false, 'the window closed after the change');
});

test('a refusal stays in the window with its reason, and nothing is reported as changed', async () => {
  closeAll();
  const { win, calls } = open({ rotate: async () => { throw new Error('bus.field_hash_key_changed'); } });
  retype(win, 'zmień klucz');
  confirm(win);
  await tick();
  await tick();
  assert.deepEqual(calls.done, []);
  assert.equal(win.isConnected, true);
  assert.equal(win.querySelector('#retype-error').hidden, false);
  assert.equal(win.querySelector('#retype-error').textContent, 'błąd: bus.field_hash_key_changed');
  assert.equal(win.querySelector('[data-action="confirm"]').hasAttribute('disabled'), false, 'the words stay typed: a second try needs no retyping');
});

test('when somebody changed the key first the window takes the current version, says so, and the second confirmation names it', async () => {
  closeAll();
  const stored = { ...INFO, keyVersion: 5, rotatedAtMs: AUG_12 + 86_400_000 };
  const sent = [];
  let reloads = 0;
  const { win, calls } = open({
    rotate: async (version) => {
      sent.push(version);
      if (version !== 5) throw new Error('protocol error Conflict: bus.field_hash_key_changed: the key is not version ' + version);
      return { key: { ...stored, keyVersion: 6 } };
    },
    reload: async () => { reloads += 1; return stored; },
  });
  retype(win, 'zmień klucz');
  confirm(win);
  await tick();
  await tick();
  assert.deepEqual(sent, [3]);
  assert.equal(reloads, 1);
  assert.equal(win.isConnected, true, 'the window stays: nothing was changed');
  assert.deepEqual(calls.done, []);
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /Obecny klucz ma wersję 5 \(ustawiony 13\.08\.2026\); nowy dostanie wersję 6\./, 'what the window lists is the key as it is now');
  const note = win.querySelector('#retype-error');
  assert.equal(note.hidden, false);
  assert.match(note.textContent, /ktoś zmienił go przed Tobą.*Okno pokazuje już aktualną wersję \(5\)/);
  const button = win.querySelector('[data-action="confirm"]');
  assert.equal(button.hasAttribute('disabled'), false, 'the words stay typed');
  confirm(win);
  await tick();
  await tick();
  assert.deepEqual(sent, [3, 5], 'the second try names the version that is stored');
  assert.deepEqual(calls.done.map((k) => k.keyVersion), [6]);
});

test('without a way to reload, or when the reload fails, the refusal is shown as it is', async () => {
  closeAll();
  const changed = async () => { throw new Error('bus.field_hash_key_changed'); };
  const noReload = open({ rotate: changed });
  retype(noReload.win, 'zmień klucz');
  confirm(noReload.win);
  await tick();
  await tick();
  assert.equal(noReload.win.querySelector('#retype-error').textContent, 'błąd: bus.field_hash_key_changed');
  closeAll();
  const failing = open({ rotate: changed, reload: async () => { throw new Error('offline'); } });
  retype(failing.win, 'zmień klucz');
  confirm(failing.win);
  await tick();
  await tick();
  assert.equal(failing.win.querySelector('#retype-error').textContent, 'błąd: bus.field_hash_key_changed');
  assert.match(norm(failing.win.querySelector('[data-role="impact"]').textContent), /Obecny klucz ma wersję 3/, 'the figures stay what the window showed');
});

test('a double click rotates once: the button is locked while the request is out', async () => {
  closeAll();
  let release;
  const pending = new Promise((resolve) => { release = resolve; });
  const { win, calls } = open({ rotate: async (version) => { calls.rotated.push(version); await pending; return { key: INFO }; } });
  retype(win, 'zmień klucz');
  confirm(win);
  confirm(win);
  await tick();
  assert.deepEqual(calls.rotated, [3]);
  assert.ok(win.querySelector('[data-action="confirm"]').hasAttribute('disabled'));
  release();
  await tick();
  await tick();
  assert.deepEqual(calls.rotated, [3]);
});

test('the words to retype and every text of the window exist in every language', async () => {
  const before = I18n.getLanguage();
  try {
    for (const lang of ['en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(lang);
      closeAll();
      const { win } = open();
      const text = norm(win.textContent);
      assert.doesNotMatch(text, /tentabus\.hiding\.key\./, `${lang}: no raw key leaks`);
      const word = norm(win.querySelector('.tb-retype-name').textContent);
      assert.ok(word.length > 3, `${lang}: there are words to retype`);
      retype(win, word);
      assert.equal(win.querySelector('[data-action="confirm"]').hasAttribute('disabled'), false, `${lang}: the retyped words unlock the button`);
      closeAll();
    }
  } finally {
    await I18n.setLanguage(before);
  }
});
