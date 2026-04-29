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
