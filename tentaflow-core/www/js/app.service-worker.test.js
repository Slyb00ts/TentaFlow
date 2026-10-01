// ============ File: app.service-worker.test.js — Authenticated entry points share one Service Worker registration ============

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

const source = readFileSync(new URL('./app.js', import.meta.url), 'utf8');
const bootstrap = source.slice(source.indexOf('async function bootstrap()'), source.indexOf('let serviceWorkerRegistration = null;'));
const registration = source.slice(source.indexOf('let serviceWorkerRegistration = null;'), source.indexOf('// The SSO callback'));
const login = source.slice(source.indexOf('function renderLogin()'), source.indexOf('async function renderApp()'));
// The shell after this boundary is independent of the authenticated registration lifecycle.
const authenticatedEntry = `${source.slice(source.indexOf('async function renderApp()'), source.indexOf('  const role = (me?.role'))}\n}`;

function client({ authenticated = false, secure = true, supported = true, registrationError = null } = {}) {
  const state = { authenticated, me: { role: 'user' }, mount: null, calls: [], warnings: [] };
  const workerRegistration = { scope: 'https://localhost/' };
  const context = {
    ApiBinary: { hasJwt: () => state.authenticated, one: async () => state.me, clearSession: () => { state.authenticated = false; } },
    navigator: supported ? { serviceWorker: { register: async (url, options) => {
      state.calls.push({ url, options: JSON.parse(JSON.stringify(options)) });
      if (registrationError) throw new Error(registrationError);
      return workerRegistration;
    } } } : {},
    window: { isSecureContext: secure },
    console: { warn: (...args) => state.warnings.push(args) },
    codecReady: Promise.resolve(),
    I18n: { init: async () => {}, applyDataI18n() {} },
    ConnectionOverlay: { init() {} }, UpdateOverlay: { init() {} }, SystemEvents: { init() {} },
    initTransport: async () => {}, takeSsoTokenFromFragment: () => null, handlePairDeepLink() {},
    byId: () => ({ innerHTML: '' }),
    LoginScreen: { render: () => '', mount: (options) => { state.mount = options; } },
  };
  runInNewContext(`${bootstrap}\n${registration}\n${login}\n${authenticatedEntry}`, context);
  return { state, context };
}

test('first interactive login registers without reloading and repeated shell renders reuse it', async () => {
  const { state, context } = client();
  await context.bootstrap();
  assert.equal(state.calls.length, 0);
  assert.equal(typeof state.mount.onSuccess, 'function');
  state.authenticated = true;
  await state.mount.onSuccess();
  await context.renderApp();
  assert.deepEqual(state.calls, [{ url: '/sw.js', options: { updateViaCache: 'none' } }]);
});

test('authenticated startup and completed password rotation use the same entry point', async () => {
  const startup = client({ authenticated: true });
  await startup.context.bootstrap();
  await Promise.resolve();
  assert.equal(startup.state.calls.length, 1);
  const rotation = client({ authenticated: true });
  rotation.state.me = { username: 'user', mustChangePassword: true };
  await rotation.context.renderApp();
  assert.equal(rotation.state.calls.length, 0);
  assert.equal(rotation.state.mount.passwordRotation, true);
  rotation.state.me = { role: 'user' };
  await rotation.state.mount.onSuccess();
  assert.equal(rotation.state.calls.length, 1);
});

test('unsupported or insecure contexts do not register, and registration failure stays observable', async () => {
  for (const options of [{ supported: false }, { secure: false }]) {
    const { state, context } = client({ authenticated: true, ...options });
    await context.renderApp();
    assert.equal(state.calls.length, 0);
  }
  const failed = client({ authenticated: true, registrationError: 'Certificate rejected' });
  await failed.context.renderApp();
  await failed.context.initializeServiceWorker();
  assert.deepEqual(failed.state.warnings, [['[app] SW register failed:', 'Certificate rejected']]);
  assert.equal(failed.state.calls.length, 1);
});
