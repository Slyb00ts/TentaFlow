// ===== File: lib/sandbox-repair.js — repairing a node's agent sandbox from the dashboard =====
//
// A node whose kernel denies bubblewrap its user namespaces (Ubuntu's AppArmor
// restriction) cannot run agents, sign in to a provider CLI or host a
// process-sandbox workspace. That is fixable with root on the node itself, so
// every screen that shows the cause offers the fix: one sudo password, the node
// installs and loads the AppArmor profile, and the answer is the sandbox as the
// node measures it afterwards.

import { escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';

/** The sandbox causes a node repairs itself when given a sudo password. */
const REPAIRABLE = new Set(['user_namespaces_denied']);

// Installing and loading a profile is quick; the margin covers a peer reached
// over the mesh.
const REPAIR_TIMEOUT_MS = 90_000;

/** Whether `cause` (a `ProcessSandboxCause` slug) has a repair button. */
export function sandboxRepairable(cause) {
  return REPAIRABLE.has(String(cause ?? ''));
}

/** The reader's sentence for a repairable cause, naming the node. */
export function sandboxRepairProblem(nodeName) {
  return I18n.t('sandbox_repair.problem_userns', { node: nodeName });
}

/**
 * Asks for `nodeName`'s sudo password and repairs its sandbox. The request is
 * addressed to the node itself (`targetNodeId` for a peer), so a remote node is
 * repaired by its own root. Resolves to `{ supported, cause, reason }` as the
 * node measured after the repair, or `null` when the operator cancelled. A
 * repair that ran but left the sandbox broken keeps the window open with the
 * node's reason, instead of reporting success.
 */
export async function repairProcessSandbox({ nodeId = null, nodeName = '', isLocal = true } = {}) {
  // Loaded on the click: the window's components are not needed to decide
  // whether a cause is repairable, which is all most screens ask.
  const { openSudoDialog } = await import('/js/lib/sudo-dialog.js');
  const options = { timeoutMs: REPAIR_TIMEOUT_MS };
  if (!isLocal && nodeId) options.targetNodeId = nodeId;
  return openSudoDialog({
    title: I18n.t('sandbox_repair.title'),
    nodeName,
    explainHtml: escapeHtml(I18n.t('sandbox_repair.explain')),
    confirmLabel: I18n.t('sandbox_repair.action'),
    confirmIcon: 'shield',
    onConfirm: async (password) => {
      const response = await ApiBinary.action(
        'codeStudioProcessSandboxRepairRequest',
        { sudoPassword: password },
        options,
      );
      const outcome = {
        supported: (response?.supportsProcessSandbox ?? response?.supports_process_sandbox) === true,
        cause: response?.processSandboxCause ?? response?.process_sandbox_cause ?? null,
        reason: response?.processSandboxReason ?? response?.process_sandbox_reason ?? '',
      };
      if (!outcome.supported) {
        throw new Error(I18n.t('sandbox_repair.still_broken', { reason: outcome.reason || outcome.cause || '?' }));
      }
      return outcome;
    },
  });
}
