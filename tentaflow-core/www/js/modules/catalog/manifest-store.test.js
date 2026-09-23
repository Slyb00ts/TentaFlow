// =============================================================================
// File: modules/catalog/manifest-store.test.js
// Description: `isManagedCliAgent` is the gate the catalog card click handler
//       uses to decide whether a tile opens the deploy wizard or the "Konta
//       agentów" screen instead — a managed-CLI coding agent (codex/claude-code/
//       grok-build/muse-code) is never deployed as a `services` row. Pinned
//       here against the exact wire shape the manifest generator emits
//       (`engine.category = "agents"`, `deploy.native.runtime = "managed-cli"`),
//       so a future rename of either field silently breaks the guard instead
//       of failing a test.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isManagedCliAgent } from './manifest-store.js';

test('a managed-CLI coding agent is flagged', () => {
  const service = {
    engine: { id: 'codex', category: 'agents' },
    deploy: { docker: null, native: { runtime: 'managed-cli' }, external: null },
  };
  assert.equal(isManagedCliAgent(service), true);
});

test('an agents-category engine on a different runtime is not flagged', () => {
  const service = {
    engine: { id: 'teams-bot', category: 'agents' },
    deploy: { docker: null, native: { runtime: 'binary' }, external: null },
  };
  assert.equal(isManagedCliAgent(service), false);
});

test('a managed-cli runtime outside the agents category is not flagged', () => {
  const service = {
    engine: { id: 'not-an-agent', category: 'llm' },
    deploy: { docker: null, native: { runtime: 'managed-cli' }, external: null },
  };
  assert.equal(isManagedCliAgent(service), false);
});

test('missing sections do not throw', () => {
  assert.equal(isManagedCliAgent(null), false);
  assert.equal(isManagedCliAgent({}), false);
  assert.equal(isManagedCliAgent({ engine: { category: 'agents' } }), false);
});
