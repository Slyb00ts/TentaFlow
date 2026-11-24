// ===== File: modules/tentabus/node-label.js — how a node is named on every TentaBus screen =====
//
// A node is named by the label the server sends (the display name the app
// knows it by). A node without one has only its id, a 64-character hex string
// that does not belong on a screen: it is shown as its first 8 characters,
// and the full id goes into a `title` where the markup can carry one. A label
// that merely repeats the id counts as no label. Every card, table, sentence
// and toast reads its node names from here, so one node never has two names.

/** Characters of an id shown when the node has no label. */
export const SHORT_ID_LENGTH = 8;

/** The first characters of a node id. */
export const shortNodeId = (id) => String(id ?? '').slice(0, SHORT_ID_LENGTH);

/** The label a node really has: not empty, not the id again. */
function realLabel(node) {
  const label = String(node?.label ?? '').trim();
  return label && label !== node?.nodeId ? label : '';
}

/** The name of a node object (`{ nodeId, label }`): its label, else the short id. */
export const nodeLabel = (node) => realLabel(node) || shortNodeId(node?.nodeId);

/** The full id to show as a tooltip beside a node's name, or `''` when the name is a label (or no id is known). */
export const nodeTitle = (node) => (realLabel(node) ? '' : String(node?.nodeId ?? ''));

/**
 * `(id) => name` over a node list: the node's label, else the short id; an
 * id the list does not know is shortened the same way, and no id is `''`.
 */
export function nodeNamer(nodes) {
  const byId = new Map((nodes || []).map((n) => [n.nodeId, n]));
  return (id) => (id ? nodeLabel(byId.get(id) || { nodeId: id }) : '');
}

/** `(id) => tooltip`: the full id of a node shown by its short id, `''` for a labelled one or no id. */
export function nodeTitler(nodes) {
  const byId = new Map((nodes || []).map((n) => [n.nodeId, n]));
  return (id) => (id ? nodeTitle(byId.get(id) || { nodeId: id }) : '');
}
