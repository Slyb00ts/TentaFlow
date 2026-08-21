// =============================================================================
// File: modules/services.row-lifecycle.test.js
// Description: What a row of the Services list promises while something is
//       happening to it. Two regressions are pinned here. (1) Deleting a service
//       used to be invisible: the row kept its normal status and a live Delete
//       button for the whole container teardown, so the only feedback a user got
//       was the temptation to click again. (2) A deploying row rendered the DB
//       snapshot from the 5 s list poll — a phase and a percentage that were
//       already stale — while the deploy window streamed the truth; the row now
//       follows the same deploymentLogStreamRequest, and that stream must be
//       opened exactly once per deployment and always cleaned up.
//       services.js imports the whole dashboard, so the functions under test are
//       cut out of the real file by brace matching and evaluated against stubs —
//       the code tested here is the code that ships.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'services.js'), 'utf8');
// The real Polish catalog answers every I18n.t call, so a key this module renders
// but nobody translated would show up as a raw path in the assertions below.
const pl = JSON.parse(readFileSync(join(here, '../../i18n/pl.json'), 'utf8'));

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

const NAMES = [
  'serviceActionKey', 'deployIdFor', 'clampPct', 'syncDeployWatchers',
  'startDeployWatcher', 'onDeployChunk', 'paintDeployProgress', 'endDeployWatcher',
  'closeDeployWatcher', 'dropDeploySubscription', 'stopDeployWatchers',
  'renderRow', 'mapStatusToChip', 'categoryChipClass',
];

const PRELUDE = `
  const escapeHtml = (v) => String(v ?? '');
  const escapeAttr = (v) => String(v ?? '');
  const SHOW_ENGINE_COL = false;
  const currentUserIsAdmin = false;
  const ManifestStore = { byId: () => null, isOnDemand: () => false };
  const nodeLabelFor = () => ({ label: 'hazai', isLocal: true, hostname: 'hazai', full: 'node-1' });
`;

/** A fresh module instance: the watcher map and the delete set are module state. */
function build(env) {
  const body = NAMES.map((n) => cut(source, n)).join('\n');
  // eslint-disable-next-line no-new-func
  // `with` binds `services`, `currentTab`, `deployWatchers` and `deletingServices`
  // to the live state object, so a test can change what the screen sees between
  // two calls exactly like a list refresh does.
  const factory = new Function(
    'ApiBinary', 'I18n', 'byId', 'CSS', 'refreshServiceList', 'state',
    `${PRELUDE}
     with (state) {
       ${body}
       return { ${NAMES.join(', ')} };
     }`,
  );
  return factory(env.ApiBinary, env.I18n, env.byId, env.CSS, env.refreshServiceList, env.state);
}

// --------------------------------------------------------------------- fakes

function lookup(root, path) {
  return path.split('.').reduce((acc, part) => (acc == null ? null : acc[part]), root) ?? null;
}

const I18n = {
  t(path, vars) {
    const value = lookup(pl, path);
    if (value === null) return path;
    if (!vars) return value;
    return Object.entries(vars).reduce(
      (acc, [k, v]) => acc.split(`{${k}}`).join(String(v)),
      value,
    );
  },
};

/** Minimal DOM: only the two live cells the row paints are addressable. */
class FakeBody {
  constructor() {
    this.cells = new Map();
  }

  addCell(selector) {
    const el = { textContent: '' };
    this.cells.set(selector, el);
    return el;
  }

  querySelectorAll(selector) {
    const el = this.cells.get(selector);
    return el ? [el] : [];
  }
}

function makeEnv({ services = [], currentTab = 'list', subscribe } = {}) {
  const calls = [];
  const body = new FakeBody();
  const env = {
    calls,
    body,
    refreshCount: 0,
    state: {
      deletingServices: new Set(),
      deployWatchers: new Map(),
      services,
      currentTab,
    },
    I18n,
    byId: () => body,
    CSS: { escape: (v) => String(v) },
    refreshServiceList: () => { env.refreshCount += 1; return Promise.resolve(); },
    ApiBinary: {
      subscribe: subscribe || ((kind, payload, handlers) => {
        const call = { kind, payload, handlers, unsubscribed: 0 };
        calls.push(call);
        call.unsubscribe = () => { call.unsubscribed += 1; };
        return Promise.resolve(call.unsubscribe);
      }),
    },
  };
  return env;
}

function svc(overrides = {}) {
  return {
    id: 7,
    node_id: 'node-1',
    display_name: 'Teams Bot',
    engine_id: 'teams-bot',
    category: 'tools',
    status: 'running',
    models: [],
    ...overrides,
  };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

// ---------------------------------------------------------------------------
// Delete feedback — the row has to say something the moment you confirm
// ---------------------------------------------------------------------------

test('a row with a delete in flight reports "deleting" instead of its old status', () => {
  const env = makeEnv();
  const mod = build(env);
  const service = svc();
  env.state.deletingServices.add(mod.serviceActionKey(service));

  const html = mod.renderRow(service);
  assert.match(html, /class="svc-row-deleting"/);
  assert.match(html, new RegExp(I18n.t('services.status.deleting')));
  assert.doesNotMatch(html, new RegExp(I18n.t('services.status.running')));
});

test('the status text rides on the chip attribute, not on chip children', () => {
  const env = makeEnv();
  const mod = build(env);
  // morphdom never descends into a tf-* element, so a status written between the
  // chip tags would freeze at whatever the chip was first built with — the row
  // would keep claiming "Działa" through the whole delete.
  const running = mod.renderRow(svc());
  assert.match(running, new RegExp(`label="${I18n.t('services.status.running')}"`));

  const service = svc();
  env.state.deletingServices.add(mod.serviceActionKey(service));
  assert.match(mod.renderRow(service), new RegExp(`label="${I18n.t('services.status.deleting')}"`));
});

test('a row with a delete in flight offers no action to click twice', () => {
  const env = makeEnv();
  const mod = build(env);
  const service = svc();

  const before = mod.renderRow(service);
  assert.match(before, /data-svc-delete="7"/);
  assert.match(before, /data-svc-pause-play/);
  assert.doesNotMatch(before, /<tf-spinner/);

  env.state.deletingServices.add(mod.serviceActionKey(service));
  const during = mod.renderRow(service);
  assert.doesNotMatch(during, /data-svc-delete/);
  assert.doesNotMatch(during, /data-svc-pause-play/);
  assert.doesNotMatch(during, /data-svc-pin-toggle/);
  assert.doesNotMatch(during, /data-svc-edit/);
  assert.match(during, /<tf-spinner/);
});

test('clearing the marker restores the row it was rendered from', () => {
  const env = makeEnv();
  const mod = build(env);
  const service = svc();
  const key = mod.serviceActionKey(service);
  const before = mod.renderRow(service);

  env.state.deletingServices.add(key);
  env.state.deletingServices.delete(key);

  assert.equal(mod.renderRow(service), before);
});

test('the delete marker is per node — the same service id on another node is untouched', () => {
  const env = makeEnv();
  const mod = build(env);
  env.state.deletingServices.add(mod.serviceActionKey(svc({ node_id: 'node-1' })));

  const other = mod.renderRow(svc({ node_id: 'node-2' }));
  assert.doesNotMatch(other, /svc-row-deleting/);
  assert.match(other, /data-svc-delete="7"/);
});

// ---------------------------------------------------------------------------
// Live deploy progress — the stream wins over the 5 s list snapshot
// ---------------------------------------------------------------------------

test('a deploying row without a stream still shows what the list snapshot knows', () => {
  const env = makeEnv();
  const mod = build(env);
  const html = mod.renderRow(svc({
    status: 'deploying',
    active_deploy_id: 'dep-1',
    progress_message: '[assets] bundle (AlreadyPresent)',
    deployment_progress_pct: 12,
  }));
  assert.match(html, /\[assets\] bundle \(AlreadyPresent\)/);
  assert.match(html, /12%/);
});

test('a deploying row prefers the streamed phase and percentage over the stale snapshot', () => {
  const env = makeEnv();
  const mod = build(env);
  env.state.deployWatchers.set('dep-1', { phase: 'pulling image', pct: 64 });

  const html = mod.renderRow(svc({
    status: 'deploying',
    active_deploy_id: 'dep-1',
    progress_message: '[assets] bundle (AlreadyPresent)',
    deployment_progress_pct: 0,
  }));
  assert.match(html, /pulling image/);
  assert.match(html, /64%/);
  assert.doesNotMatch(html, /AlreadyPresent/);
  assert.doesNotMatch(html, /0%/);
  // The live cells must be addressable by deployId, or the stream has nothing
  // to write into between two list patches.
  assert.match(html, /data-svc-deploy-phase="dep-1"/);
  assert.match(html, /data-svc-deploy-pct="dep-1"/);
});

test('a stream that reported a phase but no percentage yet falls back to the snapshot number', () => {
  const env = makeEnv();
  const mod = build(env);
  env.state.deployWatchers.set('dep-1', { phase: 'starting container', pct: null });

  const html = mod.renderRow(svc({
    status: 'deploying',
    active_deploy_id: 'dep-1',
    deployment_progress_pct: 41,
  }));
  assert.match(html, /starting container/);
  assert.match(html, /41%/);
});

test('a row that is not deploying renders no progress cells at all', () => {
  const env = makeEnv();
  const mod = build(env);
  const html = mod.renderRow(svc({ status: 'running', active_deploy_id: 'dep-1' }));
  assert.doesNotMatch(html, /data-svc-deploy-pct/);
});

// ---------------------------------------------------------------------------
// Stream bookkeeping — one subscription per deployment, always cleaned up
// ---------------------------------------------------------------------------

test('every deploying row gets exactly one stream, no matter how often the list re-renders', async () => {
  const env = makeEnv({
    services: [
      svc({ id: 1, status: 'deploying', active_deploy_id: 'dep-1' }),
      svc({ id: 2, status: 'deploying', active_deploy_id: 'dep-2' }),
      svc({ id: 3, status: 'running', active_deploy_id: 'dep-3' }),
    ],
  });
  const mod = build(env);

  // Three renders in a row, the first two while subscribe() is still pending.
  mod.syncDeployWatchers();
  mod.syncDeployWatchers();
  await flush();
  mod.syncDeployWatchers();
  await flush();

  assert.deepEqual(env.calls.map((c) => c.payload.deployId), ['dep-1', 'dep-2']);
  assert.equal(env.calls[0].kind, 'deploymentLogStreamRequest');
  // The row only needs live state; the log tail belongs to the deploy window.
  assert.equal(env.calls[0].payload.replayTail, false);
});

test('a deployment that finished is not resubscribed on the next render', async () => {
  const env = makeEnv({ services: [svc({ status: 'deploying', active_deploy_id: 'dep-1' })] });
  const mod = build(env);
  mod.syncDeployWatchers();
  await flush();

  env.calls[0].handlers.onEnd({ variant: 'DeploymentStreamEnd' });
  assert.equal(env.calls[0].unsubscribed, 1);
  assert.equal(env.refreshCount, 1, 'the verdict has to pull a fresh list');

  // The list still says "deploying" until the backend writes the final status.
  mod.syncDeployWatchers();
  await flush();
  assert.equal(env.calls.length, 1);
});

test('a row that stops deploying releases its stream', async () => {
  const env = makeEnv({ services: [svc({ status: 'deploying', active_deploy_id: 'dep-1' })] });
  const mod = build(env);
  mod.syncDeployWatchers();
  await flush();

  env.state.services = [svc({ status: 'running', active_deploy_id: 'dep-1' })];
  mod.syncDeployWatchers();

  assert.equal(env.calls[0].unsubscribed, 1);
  assert.equal(env.state.deployWatchers.size, 0);
});

test('leaving the list tab releases every stream', async () => {
  const env = makeEnv({
    services: [
      svc({ id: 1, status: 'deploying', active_deploy_id: 'dep-1' }),
      svc({ id: 2, status: 'deploying', active_deploy_id: 'dep-2' }),
    ],
  });
  const mod = build(env);
  mod.syncDeployWatchers();
  await flush();

  env.state.currentTab = 'aliases';
  mod.syncDeployWatchers();

  assert.deepEqual(env.calls.map((c) => c.unsubscribed), [1, 1]);
  assert.equal(env.state.deployWatchers.size, 0);
});

test('a stream that fails to open frees its slot for the next render', async () => {
  let attempts = 0;
  const env = makeEnv({
    services: [svc({ status: 'deploying', active_deploy_id: 'dep-1' })],
    subscribe: () => {
      attempts += 1;
      return attempts === 1 ? Promise.reject(new Error('socket down')) : Promise.resolve(() => {});
    },
  });
  const mod = build(env);

  mod.syncDeployWatchers();
  await flush();
  assert.equal(env.state.deployWatchers.size, 0);

  mod.syncDeployWatchers();
  await flush();
  assert.equal(attempts, 2);
  assert.equal(env.state.deployWatchers.size, 1);
});

test('an unsubscribe racing a pending subscribe still closes the stream', async () => {
  let resolveSubscribe;
  let unsubscribed = 0;
  const env = makeEnv({
    services: [svc({ status: 'deploying', active_deploy_id: 'dep-1' })],
    subscribe: () => new Promise((resolve) => { resolveSubscribe = resolve; }),
  });
  const mod = build(env);

  mod.syncDeployWatchers();
  env.state.currentTab = 'aliases';
  mod.syncDeployWatchers();
  resolveSubscribe(() => { unsubscribed += 1; });
  await flush();

  assert.equal(unsubscribed, 1, 'the late subscription must not survive the screen');
});

// ---------------------------------------------------------------------------
// Chunk handling — what the row takes from the stream
// ---------------------------------------------------------------------------

test('phase and progress frames update the row, log lines do not', async () => {
  const env = makeEnv({ services: [svc({ status: 'deploying', active_deploy_id: 'dep-1' })] });
  const mod = build(env);
  const phaseCell = env.body.addCell('[data-svc-deploy-phase="dep-1"]');
  const pctCell = env.body.addCell('[data-svc-deploy-pct="dep-1"]');
  mod.syncDeployWatchers();
  await flush();
  const { onChunk } = env.calls[0].handlers;

  onChunk({ variant: 'DeploymentStreamChunk', kind: 'phase', phase: 'building', progressPct: 20 });
  assert.equal(phaseCell.textContent, 'building');
  assert.equal(pctCell.textContent, '20%');

  onChunk({ variant: 'DeploymentStreamChunk', kind: 'progress', progressPct: 55, line: '#7 DONE' });
  assert.equal(phaseCell.textContent, 'building', 'a progress frame without a phase keeps the last one');
  assert.equal(pctCell.textContent, '55%');

  onChunk({ variant: 'DeploymentStreamChunk', kind: 'log', line: 'Step 3/9 : RUN pip install' });
  assert.equal(phaseCell.textContent, 'building');
  assert.equal(pctCell.textContent, '55%');
});

test('a chunk addressed to another deployment is ignored', async () => {
  const env = makeEnv({ services: [svc({ status: 'deploying', active_deploy_id: 'dep-1' })] });
  const mod = build(env);
  const pctCell = env.body.addCell('[data-svc-deploy-pct="dep-1"]');
  mod.syncDeployWatchers();
  await flush();

  env.calls[0].handlers.onChunk({
    variant: 'DeploymentStreamChunk', kind: 'progress', deployId: 'dep-9', progressPct: 90,
  });
  assert.equal(pctCell.textContent, '');
  assert.equal(env.state.deployWatchers.get('dep-1').pct, null);
});

test('a percentage outside 0..100 is clamped before it reaches the cell', () => {
  const env = makeEnv();
  const mod = build(env);
  assert.equal(mod.clampPct(-5), 0);
  assert.equal(mod.clampPct(140), 100);
  assert.equal(mod.clampPct(33.6), 34);
  assert.equal(mod.clampPct(undefined), 0);
});

test('deployIdFor prefers the active deployment over the last finished one', () => {
  const env = makeEnv();
  const mod = build(env);
  assert.equal(mod.deployIdFor({ active_deploy_id: 'a', last_deploy_id: 'b' }), 'a');
  assert.equal(mod.deployIdFor({ last_deploy_id: 'b' }), 'b');
  assert.equal(mod.deployIdFor({}), '');
});
