// ============================================================================
// File: coding-agent.js — one node-routed RPC to a coding-agent SERVICE.
//
// What is left here is the bridge's own console protocol (`session.*`,
// `models.list`, `auth.status`), which the Services console and Code Studio
// still speak. An ACCOUNT is no longer a service: its identity, grants,
// credential and sign-in live in the `ProviderAccountBody` family and in the
// screens built on it (modules/agent-accounts*.js).
// ============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

export async function agentRequest(service, operation, payload = {}) {
  const response = await ApiBinary.action('serviceAgentRequest', {
    serviceId: Number(service.id),
    nodeId: service.nodeId || service.node_id,
    operation,
    payloadJson: JSON.stringify(payload),
  });
  if (!response?.success) {
    const error = response?.error || 'Coding-agent request failed';
    if (error.includes('session_expired')) {
      throw new Error('Sesja CLI wygasła. Ponowne logowanie może wykonać administrator.');
    }
    if (error.includes('administrator_required_for_login')) {
      throw new Error('Ponowne logowanie jest dostępne wyłącznie dla administratora.');
    }
    throw new Error(error);
  }
  return JSON.parse(response.resultJson || response.result_json || '{}');
}
