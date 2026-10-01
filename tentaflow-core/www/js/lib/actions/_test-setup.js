// ===== File: lib/actions/_test-setup.js — DOM, English strings and window plumbing for the action module tests =====

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver || class { observe() {} disconnect() {} };
}
if (typeof globalThis.MutationObserver === 'undefined') globalThis.MutationObserver = window.MutationObserver;
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const english = readFileSync(fileURLToPath(new URL('../../../i18n/en.json', import.meta.url)), 'utf8');
const otherFetch = globalThis.fetch;
globalThis.fetch = (url, init) => {
  const href = String(url);
  if (href.endsWith('/i18n/en.json')) return Promise.resolve({ ok: true, json: () => Promise.resolve(JSON.parse(english)) });
  if (href.startsWith('file:')) return otherFetch(url, init);
  return Promise.resolve({ ok: true, text: () => Promise.resolve('') });
};
globalThis.localStorage ??= window.localStorage;
globalThis.navigator ??= window.navigator;
localStorage.setItem('tentaflow_lang', 'en');
const { I18n } = await import('/js/i18n.js');
await I18n.init();
if (I18n.t('actions.cancel') !== 'Cancel') throw new Error('English strings did not load for the action tests');

export { window, I18n };

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Lets a window finish closing (tf-window removes itself after its closing animation). */
export const closed = () => sleep(300);

export const key = (target, name, extra = {}) => {
  const event = new window.KeyboardEvent('keydown', { key: name, bubbles: true, cancelable: true, ...extra });
  target.dispatchEvent(event);
  return event;
};

export function cleanBody() {
  document.body.innerHTML = '';
}

export const people = [
  { id: 'u1', name: 'Anna Kowalska', role: 'PM', load: 70 },
  { id: 'u2', name: 'Marek Nowak', role: 'Developer', load: 95 },
  { id: 'u3', name: 'Piotr Zieliński', role: 'Developer', load: 130 },
  { id: 'u4', name: 'Paweł Szymański', role: 'Developer', load: 80, suggestion: 'module deputy' },
  { id: 'u5', name: 'Ewa Wiśniewska', role: 'Tester', load: 85, absence: 'away until 24.10, covered by Paweł S.' },
];

export const agents = [
  { id: 'a1', name: 'Coding agent', role: 'local model' },
];
