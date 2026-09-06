// =============================================================================
// File: modules/tentaquant/targets.test.js
// Description: The "Uruchom na…" model. Two things must be true of it and are
// easy to get wrong: a target the server refuses stays VISIBLE with the
// server's own reason (plan §4.1 — a tier that vanishes silently is what this
// list exists to prevent), and `auto` is never decided here — it is resolved by
// `Target::Resolve` and only reported.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  AUTO_TARGET, BROWSER_TARGET, autoHint, canStart, chooseTarget, effectiveTarget, isBrowserTarget,
  resolvedNodeName, startRefusal, targetByValue, targetLabel, targetOptions,
} = await import('./targets.js');

/// What `Target::Resolve` actually answers (`TargetResolveResponse`): a target,
/// a tier, a node ID — an iroh public key — and NO name. Every test below
/// resolves with this shape, because a hint tested against an invented
/// `nodeName` proves nothing about the wire.
const NODE_A_ID = '7f'.repeat(32);
const resolvedToCore = (over = {}) => ({
  target: 'core:node-a', tier: 'T1', nodeId: NODE_A_ID, reason: 'wider than the browser tier', unavailable: [], ...over,
});

const browser = {
  target: 'browser', tier: 'T0', nodeId: null, nodeName: 'browser', isLocal: true,
  online: true, available: true, maxQubits: 24, precision: 'single', reason: null,
};
const local = {
  target: 'core:node-a', tier: 'T1', nodeId: 'node-a', nodeName: 'node-a', isLocal: true,
  online: true, available: true, maxQubits: 28, precision: 'double', reason: null,
};
const remote = {
  target: 'core:node-b', tier: 'T1', nodeId: 'node-b', nodeName: 'node-b', isLocal: false,
  online: true, available: false, maxQubits: 28, precision: 'double',
  reason: 'runs on another node cannot stream their evolution here yet',
};
const TARGETS = [browser, local, remote];

test('every target is offered, and `auto` leads', () => {
  const options = targetOptions(TARGETS);
  assert.deepEqual(options.map((o) => o.value), [AUTO_TARGET, 'browser', 'core:node-a', 'core:node-b']);
  assert.deepEqual(options.map((o) => o.disabled), [false, false, false, true]);
});

test('a refused target keeps the server\'s reason in the text a person reads', () => {
  const label = targetLabel(remote);
  assert.match(label, /T1 · Core · node-b/);
  assert.match(label, /runs on another node cannot stream their evolution here yet/);
  // A tooltip would be invisible on a phone and to a reader that only reads the
  // option, so the reason is in the label itself.
  assert.equal(targetOptions(TARGETS)[3].label, label);
});

test('a target labels the width it actually refuses above', () => {
  assert.match(targetLabel(browser), /T0 · przeglądarka \(≤ 24 kubity\)/);
  assert.match(targetLabel(local), /T1 · Core · node-a \(≤ 28 kubitów\)/);
});

test('a target that is gone or refused falls back to `auto`, never to another node', () => {
  assert.equal(chooseTarget(TARGETS, 'core:node-a'), 'core:node-a');
  assert.equal(chooseTarget(TARGETS, 'core:node-b'), AUTO_TARGET, 'refused');
  assert.equal(chooseTarget(TARGETS, 'core:node-gone'), AUTO_TARGET, 'no longer listed');
  assert.equal(chooseTarget([], 'browser'), AUTO_TARGET);
});

test('the hint reports the rule the server evaluated, and says when it is still asking', () => {
  assert.equal(autoHint(null, TARGETS), 'auto → sprawdzam…');
  assert.equal(autoHint({ target: 'browser', tier: 'T0', nodeId: null }, TARGETS), 'auto → T0 · przeglądarka');
  assert.equal(autoHint(resolvedToCore(), TARGETS), 'auto → T1 · node-a');
  assert.equal(autoHint({ target: '', tier: 'none', reason: 'too wide' }, TARGETS), 'auto → żadna warstwa nie przyjmie tego obwodu');
});

test('the node of an `auto` resolution is NAMED, never printed as its key', () => {
  // The resolution carries the id only; the name is a field of `TargetInfo`,
  // in the very list the select was built from.
  assert.equal(resolvedNodeName(resolvedToCore(), TARGETS), 'node-a');
  assert.doesNotMatch(autoHint(resolvedToCore(), TARGETS), /[0-9a-f]{16}/, 'no wall of hex under the field');
  // A list older than the rule's answer still cannot print 64 characters.
  assert.equal(resolvedNodeName(resolvedToCore({ target: 'core:node-new' }), TARGETS), '7f7f7f7f');
  assert.equal(resolvedNodeName({ target: 'browser', tier: 'T0', nodeId: null }, TARGETS), 'przeglądarka');
});

test('`auto` runs where the rule said, a named target runs where it says', () => {
  assert.equal(effectiveTarget(AUTO_TARGET, { target: 'core:node-a' }), 'core:node-a');
  assert.equal(effectiveTarget('browser', { target: 'core:node-a' }), 'browser');
  assert.equal(effectiveTarget(AUTO_TARGET, null), '', 'unresolved names no target');
});

test('an unresolved `auto` still starts — this page IS the browser tier', () => {
  assert.equal(canStart(TARGETS, AUTO_TARGET, null), true);
  assert.equal(isBrowserTarget(effectiveTarget(AUTO_TARGET, null)), true);
});

test('a rule that found no tier stops the run before the wire refuses it', () => {
  assert.equal(canStart(TARGETS, AUTO_TARGET, { target: '', tier: 'none' }), false);
  assert.equal(canStart(TARGETS, 'core:node-b', null), false, 'a refused target cannot start one either');
  assert.equal(canStart(TARGETS, 'core:node-a', null), true);
  assert.equal(canStart(TARGETS, BROWSER_TARGET, null), true);
});

test('a target is looked up in the list, never guessed from the string', () => {
  assert.equal(targetByValue(TARGETS, 'core:node-b'), remote);
  assert.equal(targetByValue(TARGETS, 'nope'), null);
});

test('a selection that cannot start says why in the words the user should read', () => {
  assert.equal(startRefusal(TARGETS, AUTO_TARGET, null), '', 'an unresolved auto can start');
  assert.equal(
    startRefusal(TARGETS, AUTO_TARGET, { target: '', tier: 'none' }),
    'auto → żadna warstwa nie przyjmie tego obwodu',
  );
  assert.match(startRefusal(TARGETS, 'core:node-b', null), /runs on another node cannot stream/);
  assert.equal(startRefusal(TARGETS, 'core:node-gone', null), 'nieznany cel: core:node-gone');
});
