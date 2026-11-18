import '../tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { editorFields, specFromValues, limitAndWindowProblem } = await import('./policy-editor.js');

test('the editor offers every field of the spec, filled from the policy being edited', () => {
  const fields = editorFields({ name: 'Std', maxConcurrency: 4, callWindowSecs: 60, ordering: 'none' });
  const byKey = Object.fromEntries(fields.map((f) => [f.key, f]));
  assert.equal(byKey.name.value, 'Std');
  assert.equal(byKey.max_concurrency.value, 4);
  assert.equal(byKey.call_window.value, '60');
  assert.deepEqual(byKey.call_window.options.map((o) => o.value), ['1', '60', '3600', '86400']);
  assert.equal(byKey.ordering.value, 'none');
  assert.equal(editorFields().find((f) => f.key === 'ordering').value, 'partition');
});

test('a call limit and its window go together', () => {
  assert.equal(limitAndWindowProblem({ call_limit: 10, call_window: null }).field, 'call_window');
  assert.equal(limitAndWindowProblem({ call_limit: null, call_window: '60' }).field, 'call_limit');
  assert.equal(limitAndWindowProblem({ call_limit: 10, call_window: '60' }), null);
  assert.equal(limitAndWindowProblem({ call_limit: null, call_window: null }), null);
});

test('specFromValues turns the window into seconds and defaults the ordering', () => {
  const spec = specFromValues({ name: 'A', description: '', call_window: '3600', max_concurrency: 2 });
  assert.equal(spec.callWindowSecs, 3600);
  assert.equal(spec.ordering, 'partition');
  assert.equal(spec.maxConcurrency, 2);
  assert.equal(specFromValues({ name: 'A', call_window: null }).callWindowSecs, null);
});
