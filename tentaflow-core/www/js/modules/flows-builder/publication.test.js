import '../tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const p = await import('./publication.js');
const { publishedChipHtml, execMetaKey } = await import('../flows.js');

const tick = () => new Promise((resolve) => setTimeout(resolve, 20));
const flow = { id: 'f1', name: 'Camera Analysis' };

function open({ current = null, call = async () => ({}), readList = async () => ({ flows: [] }), onSaved = () => {} } = {}) {
  const win = p.openPublicationWindow({ flow, current, onSaved, deps: { call, readList } });
  const toggle = win.querySelector('tf-toggle');
  const nameInput = win.querySelector('[data-field="model"] tf-input');
  const address = [...win.querySelectorAll('tf-input')].find((el) => el.getAttribute('readonly') !== null);
  const submit = () => win.querySelector('[data-act="submit"]').click();
  return { win, toggle, nameInput, address, submit };
}

const turnOn = (toggle) => {
  toggle.setAttribute('checked', '');
  toggle.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked: true } }));
};

const setName = (nameInput, value) => {
  nameInput.setAttribute('value', value);
  nameInput.value = value;
  nameInput.dispatchEvent(new window.Event('input', { bubbles: true }));
};

test('the window opens with the flow, the toggle and the name prefilled from the current publication', () => {
  const { win, toggle, nameInput, address } = open({ current: 'camera-analysis' });
  assert.match(win.textContent, /Przepływ: Camera Analysis/);
  assert.ok(toggle.hasAttribute('checked'));
  assert.equal(toggle.getAttribute('label'), 'Udostępnij jako model w API /v1');
  assert.equal(nameInput.getAttribute('value'), 'camera-analysis');
  assert.equal(nameInput.getAttribute('placeholder'), 'np. rejestracja-api');
  assert.match(nameInput.getAttribute('hint'), /Nazwa musi być unikalna; usunięcie nazwy wycofuje publikację\./);
  assert.equal(address.getAttribute('value'), 'POST /v1/chat/completions · model: camera-analysis');
  assert.ok(!nameInput.hasAttribute('disabled'));
  win.close(true);
  const off = open({ current: null });
  assert.ok(!off.toggle.hasAttribute('checked'));
  assert.ok(off.nameInput.hasAttribute('disabled'), 'the name is enabled only while the toggle is on');
  off.win.close(true);
});

test('turning the toggle off and saving sends an empty name, which unpublishes', async () => {
  const sent = [];
  const { win, toggle, nameInput, submit } = open({ current: 'camera-analysis', call: async (payload) => { sent.push(payload); return {}; } });
  toggle.removeAttribute('checked');
  toggle.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked: false } }));
  assert.ok(nameInput.hasAttribute('disabled'));
  submit();
  await new Promise((resolve) => setTimeout(resolve, 400));
  assert.deepEqual(sent, [{ flowId: 'f1', publishedModelName: '' }]);
  assert.equal(win.isConnected, false, 'the window closes after the save');
});

test('what changes is sent and nothing else; an unchanged window sends nothing', () => {
  assert.deepEqual(p.publicationChange(null, { enabled: true, name: ' api ' }), { publishedModelName: 'api' });
  assert.deepEqual(p.publicationChange('a', { enabled: true, name: 'b' }), { publishedModelName: 'b' });
  assert.equal(p.publicationChange('a', { enabled: true, name: 'a' }), null);
  assert.deepEqual(p.publicationChange('a', { enabled: false, name: 'a' }), { publishedModelName: '' });
  assert.equal(p.publicationChange(null, { enabled: false, name: '' }), null);
});

test('a name clash is shown on the name field in our words, with the other flow named when known', async () => {
  const taken = Object.assign(new Error("flow publish name 'default-chat' is already used by another flow"), { code: 'bad_request' });
  const readList = async () => ({ flows: [{ id: 'f9', name: 'Default Chat', publishedModelName: 'default-chat' }, { id: 'f1', name: 'Me', publishedModelName: null }] });
  const { win, nameInput, toggle, submit } = open({ current: null, call: async () => { throw taken; }, readList });
  turnOn(toggle);
  setName(nameInput, 'default-chat');
  submit();
  await tick();
  assert.equal(nameInput.getAttribute('error'), 'Nazwa modelu jest już zajęta przez przepływ „Default Chat”.');
  assert.equal(win.isConnected, true, 'the window stays open');
  assert.doesNotMatch(win.textContent + nameInput.getAttribute('error'), /already used|clash|flow publish name/);
  win.close(true);
  const unknown = open({ current: null, call: async () => { throw taken; }, readList: async () => { throw new Error('down'); } });
  turnOn(unknown.toggle);
  setName(unknown.nameInput, 'default-chat');
  unknown.submit();
  await tick();
  assert.equal(unknown.nameInput.getAttribute('error'), 'Nazwa modelu jest już zajęta przez inny przepływ.');
  unknown.win.close(true);
});

test('the refusals are classified from the code and text, and an alias clash has its own wording', () => {
  const alias = p.nameClash(Object.assign(new Error("flow publish name 'x' clashes with an active alias"), { code: 'bad_request' }));
  assert.equal(alias.kind, 'alias');
  assert.match(p.clashMessage(alias), /alias modelu/);
  assert.equal(p.nameClash(Object.assign(new Error("flow publish name 'x' is already used by another flow"), { code: 10 })).kind, 'flow');
  assert.equal(p.nameClash(Object.assign(new Error('something else'), { code: 'bad_request' })), null);
  assert.equal(p.nameClash(Object.assign(new Error("flow publish name 'x' is already used by another flow"), { code: 'internal' })), null);
  const named = p.nameClash(Object.assign(new Error("flow publish name 'x' is already used by flow 'Default Chat'"), { code: 'bad_request' }));
  assert.equal(p.clashMessage(named), 'Nazwa modelu jest już zajęta przez przepływ „Default Chat”.');
});

test('the name is checked before anything is sent: required while on, length and control characters', async () => {
  assert.equal(p.modelNameProblem(''), 'required');
  assert.equal(p.modelNameProblem('   '), 'required');
  assert.equal(p.modelNameProblem('a'.repeat(65)), 'too_long');
  assert.equal(p.modelNameProblem('a'.repeat(64)), null);
  assert.equal(p.modelNameProblem('bad\nname'), 'characters');
  assert.equal(p.modelNameProblem('rejestracja-api'), null);
  const sent = [];
  const { win, nameInput, toggle, submit } = open({ current: null, call: async (payload) => { sent.push(payload); return {}; } });
  toggle.setAttribute('checked', '');
  toggle.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked: true } }));
  submit();
  await tick();
  assert.deepEqual(sent, [], 'nothing is sent for an empty name');
  assert.equal(nameInput.getAttribute('error'), 'Podaj nazwę modelu albo wyłącz udostępnianie.');
  win.close(true);
});

test('the list chip reads "/v1 · name" and keeps the case of the name', () => {
  const html = publishedChipHtml({ id: 'f1', publishedModelName: 'Camera-Analysis' });
  const holder = document.createElement('div');
  holder.innerHTML = html;
  const chip = holder.querySelector('tf-chip');
  assert.equal(chip.textContent, '/v1 · Camera-Analysis');
  assert.equal(chip.getAttribute('variant'), 'outline', 'the outline pill is sentence-case, the plain one upper-cases its text');
  assert.doesNotMatch(html, /text-transform|toUpperCase/);
  assert.equal(publishedChipHtml({ id: 'f2', publishedModelName: null }), '');
  assert.equal(publishedChipHtml({ id: 'f2', publishedModelName: '  ' }), '');
  assert.match(publishedChipHtml({ id: 'f3', publishedModelName: '<b>x</b>' }), /&lt;b&gt;x&lt;\/b&gt;/);
});

test('the history header says "latest" only when a full page was loaded', () => {
  assert.equal(execMetaKey(5), 'flows.exec_meta');
  assert.equal(execMetaKey(99), 'flows.exec_meta');
  assert.equal(execMetaKey(100), 'flows.exec_meta_latest');
});
