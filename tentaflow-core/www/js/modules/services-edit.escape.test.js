import './tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
ApiBinary.action = () => Promise.resolve({ models: [] });
ApiBinary.one = () => Promise.resolve({ rows: [] });

const { openEditModal } = await import('./services-edit.js');

const external = { id: 7, engineId: 'openai', displayName: 'Cloud', deployMethod: 'external' };
const escape = () => {
  const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
  document.body.dispatchEvent(event);
  return event;
};

test('Escape closes the edit modal and goes no further', async () => {
  await openEditModal(external);
  assert.ok(document.querySelector('.modal-overlay'));
  let reachedDocument = false;
  const onDocument = () => { reachedDocument = true; };
  document.addEventListener('keydown', onDocument);
  const event = escape();
  document.removeEventListener('keydown', onDocument);
  assert.equal(document.querySelector('.modal-overlay'), null);
  assert.equal(event.defaultPrevented, true);
  assert.equal(reachedDocument, false, 'a window the services screen lives in must not see this Escape');
});

test('a modal window opened on top owns Escape; the edit modal stays open under it', async () => {
  await openEditModal(external);
  const confirm = document.createElement('tf-window');
  confirm.setAttribute('modal', '');
  document.body.appendChild(confirm);
  escape();
  assert.ok(document.querySelector('.modal-overlay'), 'the confirmation answers Escape, not the modal under it');
  confirm.remove();
  escape();
  assert.equal(document.querySelector('.modal-overlay'), null);
});

test('once closed, Escape no longer reaches the modal handler', async () => {
  await openEditModal(external);
  escape();
  const event = escape();
  assert.equal(event.defaultPrevented, false);
});
