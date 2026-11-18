// ===== File: modules/sla/flow-list.js — the flow list, which takes no payload =====
//
// `flowListRequest` has no body, so it cannot go through `ApiBinary.action(kind, payload, options)`:
// the payload would land in the encoder's sequence slot. Its request options (a node to forward to,
// a deadline) travel as the trailing options object that `ApiBinary.one` understands.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

/** `FlowListResponse` of this node, or of `targetNodeId` when one is named. */
export function readFlowList({ targetNodeId, timeoutMs } = {}) {
  const options = { _isRequestOptions: true };
  if (typeof timeoutMs === 'number') options.timeoutMs = timeoutMs;
  if (typeof targetNodeId === 'string' && targetNodeId) options.targetNodeId = targetNodeId;
  return Object.keys(options).length > 1 ? ApiBinary.one('flowListRequest', options) : ApiBinary.one('flowListRequest');
}
