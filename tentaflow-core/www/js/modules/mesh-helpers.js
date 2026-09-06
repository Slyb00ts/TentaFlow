// === File: mesh-helpers.js — connection state helpers (PeerRegistry shape) ===
//
// PR3 — backend MeshNodeInfo no longer carries a flat `status: string`. The
// authoritative connection info lives in `node.connection`:
//   { state: 'connected'|'connecting'|..., path: { kind, addr|url }, sinceMs,
//     lastAppHeartbeatMs, transport, address, relayUrl, paths: [...] }
//
// Helpers below centralize the state checks so feature modules don't reimplement
// the matrix of online/offline labels.

export function isOnline(node) {
  if (!node) return false;
  if (node.is_local || node.isLocal) return true;
  return node.connection?.state === 'connected';
}

export function isDegraded(node) {
  return node?.connection?.state === 'degraded';
}

export function isOffline(node) {
  const s = node?.connection?.state;
  return s === 'offline' || s === 'reconnecting' || s === 'disconnected';
}

export function connStateLabel(node) {
  return node?.connection?.state ?? 'unknown';
}

export function connPathKind(node) {
  return node?.connection?.path?.kind ?? null;
}

export function connPathDisplay(node) {
  const p = node?.connection?.path;
  if (!p) return '';
  if (p.kind === 'direct') return p.addr || '';
  if (p.kind === 'relay') return p.url || '';
  return '';
}

// ---- Node registry (sync_nodes.node_kind / sync_nodes.operator) ------------

/// Device kinds `sync_nodes.node_kind` accepts, in the order the Mesh screen
/// offers them. Mirrors the column's CHECK constraint; anything outside it is
/// refused by the handler before it reaches SQLite.
export const NODE_KINDS = [
  'unknown',
  'phone',
  'tablet',
  'laptop',
  'desktop',
  'server',
  'shared',
  'authority',
];

/// What the device kind SUGGESTS about the operator flag, or `null` where it
/// suggests nothing (`unknown`, `laptop`, `shared` — a laptop is as plausibly a
/// workstation as a carry-around).
///
/// A suggestion only. Changing the kind never moves the flag by itself: the kind
/// is a description the node states about itself, the flag is authority over the
/// organization, and making one a side effect of the other would let a cosmetic
/// edit hand out that authority.
export function operatorHintFor(nodeKind) {
  switch (nodeKind) {
    case 'desktop':
    case 'server':
    case 'authority':
      return true;
    case 'phone':
    case 'tablet':
      return false;
    default:
      return null;
  }
}
