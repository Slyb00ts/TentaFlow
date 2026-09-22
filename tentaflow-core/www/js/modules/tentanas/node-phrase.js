// ===== File: modules/tentanas/node-phrase.js — a sentence that names a node, for a named and a nameless node alike =====
//
// WHY a chooser and not `nodeLabel`. `nodeLabel` falls back to
// "node.unnamed", which is itself a phrase with the noun in it ("Węzeł bez
// nazwy"). Dropped into a template that already says "węzeł {node}" /
// "na węźle {node}" / "{user}@{node}", it doubles the noun or breaks the
// grammar: "Pule na węźle Węzeł bez nazwy", "tentaflow@Węzeł bez nazwy".
// So each such template has a sibling `<key>_unnamed` written as a whole
// sentence for a node with no name ("Pule na węźle bez nazwy"), and this
// picks between the two.
//
// A template that only sets the name apart ("Hasło sudo — {node}") reads
// fine with the fallback label and keeps using `nodeLabel`.

import { T } from '/js/modules/tentanas/format.js';

/**
 * `T(key, { ...params, node: <name> })` for a node with a name, and
 * `T(key + '_unnamed', params)` for one without (or no node at all).
 *
 * A key with no dedicated `_unnamed` sibling falls back to the plain
 * "node.unnamed" phrase instead of leaking the raw
 * "tentanas.<key>_unnamed" string onto the screen — the header subline
 * (`nodeHeadSub` below) has no sibling key of its own because the bare
 * "Węzeł bez nazwy" is already the whole sentence it needs.
 */
export function nodeT(key, node, params = {}) {
  const name = String(node?.nodeName || '').trim();
  if (name) return T(key, { ...params, node: name });
  const unnamedKey = key + '_unnamed';
  const label = T(unnamedKey, params);
  return label === 'tentanas.' + unnamedKey ? T('node.unnamed') : label;
}

// The node view's header subline: "węzeł {name}" for a named node, the bare
// "Węzeł bez nazwy" for one with none — folding the fallback into the
// template would double the noun, same as every other phrase in this file.
// This used to be its own copy of the same chooser, in format.js.
// `node.head_sub_node` is the one template in this family that names its
// placeholder `{name}` rather than `{node}`, so the name is passed through
// `params` too; `nodeT` still adds its own `node` param, which the template
// simply does not use.
export function nodeHeadSub(node) {
  const name = String(node?.nodeName || '').trim();
  return nodeT('node.head_sub_node', node, { name });
}
