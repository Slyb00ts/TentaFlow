// ============ File: flows-builder/process-retry.js — what an incident says about its retry: the offer and, when there is none, the reason in words ============

import { I18n } from '/js/i18n.js';

const text = (key, values) => I18n.t(`bpmn.${key}`, values);

// One sentence per reason the server can give; the client never shows the reason's name.
const REASON_KEYS = {
  ViewOnly: 'retry_denied_view_only',
  InitiatorNotOwner: 'retry_denied_initiator_not_owner',
  InitiatorUnavailable: 'retry_denied_initiator_unavailable',
  SourceUnavailable: 'retry_denied_source_unavailable',
  ActivationBusy: 'retry_denied_activation_busy',
  NoProofOfNoEffect: 'retry_denied_no_proof_of_no_effect',
  OutcomeUncertain: 'retry_denied_outcome_uncertain',
  SameFailureRepeats: 'retry_denied_same_failure_repeats',
  NeedsPerson: 'retry_denied_needs_person',
  Automatic: 'retry_denied_automatic',
  OtherMechanism: 'retry_denied_other_mechanism',
};

export const RETRY_REASONS = Object.keys(REASON_KEYS);

/** The retry button is shown only for a task incident the server offers to this reader. */
export function retryOffered(incident) {
  return incident.canRetry === true && Boolean(incident.jobId);
}

/**
 * Why this reader is not offered a retry, as a sentence in their language; null
 * when it is offered, or when the server gave no reason (an old server, a
 * resolved incident, a code nobody classified).
 */
export function retryRefusal(incident) {
  if (retryOffered(incident)) return null;
  const retry = incident.retry;
  if (retry?.deniedReason) return text(REASON_KEYS[retry.deniedReason] ?? 'retry_denied_generic');
  if (retry?.kind === 'OutputMapping') return text('retry_output_mapping_pending');
  return null;
}
