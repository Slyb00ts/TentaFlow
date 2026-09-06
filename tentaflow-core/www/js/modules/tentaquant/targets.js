// ===== File: modules/tentaquant/targets.js — where a run may be placed =====
//
// `Target::List` answers every target the laboratory knows: the browser (T0)
// and Core on each node of the fleet (T1). A target that cannot take a run
// right now is NOT dropped from the list — it arrives with `available: false`
// and the reason the server wrote — so the select shows it disabled with that
// sentence instead of leaving the user to guess why the fleet shrank.
//
// The reason travels verbatim into the option label rather than into a title:
// a tooltip is invisible on a phone and to a screen reader that only reads the
// option text, and the whole point of the field is that the refusal is read.
//
// `auto` is the one option this screen does not decide: plan §5.3 puts the rule
// on the server (`Target::Resolve`) so the browser, the SDK and a notebook all
// place a run the same way. The select therefore carries `auto` as a value and
// the RESOLUTION as a hint next to it ("auto → T1 · node-a"), fetched before
// the run starts.

import { T, shortId } from '/js/modules/tentaquant/format.js';

export const AUTO_TARGET = 'auto';
export const BROWSER_TARGET = 'browser';

/// Whether a chosen value runs in this browser (tier T0). `auto` is not a
/// target: it is resolved first, and the resolution answers this question — an
/// EMPTY value is the unresolved case and stays here, in the page that can
/// always run T0.
export function isBrowserTarget(target) {
  return !target || target === BROWSER_TARGET;
}

export function targetByValue(targets, value) {
  return (targets || []).find((t) => t.target === value) || null;
}

/// One option's text: what the target is, how wide it goes, and — when it is
/// refused — the server's own words for why.
export function targetLabel(target) {
  const q = Number(target.maxQubits ?? target.max_qubits) || 0;
  const node = target.nodeName || target.node_name || target.nodeId || target.node_id || '';
  const base = String(target.tier) === 'T0'
    ? T('targets.browser', { q })
    : T('targets.core', { node, q });
  if (target.available) return base;
  return T('targets.unavailable', {
    label: base,
    reason: target.reason || T('targets.no_reason'),
  });
}

/// The option list for `tf-select.setOptions`. `auto` leads, because it is the
/// answer for somebody who does not want to think about tiers at all.
export function targetOptions(targets) {
  return [
    { value: AUTO_TARGET, label: T('targets.auto'), disabled: false },
    ...(targets || []).map((target) => ({
      value: target.target,
      label: targetLabel(target),
      disabled: !target.available,
    })),
  ];
}

/// The value the select may stand on. A wanted target that is gone or refused
/// falls back to `auto` — never to a silently different node.
export function chooseTarget(targets, wanted) {
  const found = targetByValue(targets, wanted);
  return found && found.available ? found.target : AUTO_TARGET;
}

/// The node an `auto` resolution landed on, named the way the laboratory names
/// it. `Target::Resolve` answers a node ID and nothing else — an iroh public
/// key, 64 hex characters — while the NAME is a field of `TargetInfo`, in the
/// very list this select was built from. So the id is looked up there; a node
/// that is not in the list (a list older than the rule's answer) keeps the head
/// of its id, because a wall of hex is not a name anybody reads.
export function resolvedNodeName(resolution, targets) {
  if (String(resolution.tier || '') === 'T0') return T('targets.node_browser');
  const found = targetByValue(targets, resolution.target);
  const name = found && (found.nodeName || found.node_name);
  return name || shortId(resolution.nodeId ?? resolution.node_id);
}

/// The hint under the select. Only `auto` has one: a named target IS its own
/// explanation, and repeating it under the field says nothing new.
export function autoHint(resolution, targets) {
  if (!resolution) return T('targets.auto_checking');
  const tier = String(resolution.tier || '');
  if (!resolution.target || tier === 'none') return T('targets.auto_none');
  return T('targets.auto_resolved', { tier, node: resolvedNodeName(resolution, targets) });
}

/// What a run started with this selection actually executes on. `auto` becomes
/// the resolved target; anything else is itself.
export function effectiveTarget(selected, resolution) {
  if (selected !== AUTO_TARGET) return selected;
  return resolution && resolution.target ? String(resolution.target) : '';
}

/// Why a selection cannot start a run, in the words the user should read: the
/// `auto` rule's own answer, or the refused target's reason. Empty when the
/// selection CAN start one.
export function startRefusal(targets, selected, resolution) {
  if (canStart(targets, selected, resolution)) return '';
  if (selected === AUTO_TARGET) return autoHint(resolution, targets);
  const found = targetByValue(targets, selected);
  return found ? targetLabel(found) : T('targets.unknown', { target: selected });
}

/// Whether the selection can start a run at all. `auto` that resolved to
/// NOTHING is the one case where the button has to stay down; `auto` that has
/// not been resolved YET is not that case — the browser tier is this page and
/// needs no server to confirm it, so an unanswered rule falls back to T0
/// exactly as rule 1 of plan §5.3 would.
export function canStart(targets, selected, resolution) {
  if (selected === AUTO_TARGET && !resolution) return true;
  const target = effectiveTarget(selected, resolution);
  if (!target) return false;
  if (isBrowserTarget(target)) return true;
  const found = targetByValue(targets, target);
  return Boolean(found && found.available);
}
