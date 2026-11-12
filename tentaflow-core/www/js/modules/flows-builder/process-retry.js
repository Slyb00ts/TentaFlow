// ============ File: flows-builder/process-retry.js — what an incident says about its retry: the offer and, when there is none, the reason in words ============

import { I18n } from '/js/i18n.js';
import { fmtDuration } from '/js/utils.js';

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
  OtherMechanism: 'retry_denied_other_mechanism',
  NeedsAdministrator: 'retry_denied_needs_administrator',
  ExecutionUnavailable: 'retry_denied_execution_unavailable',
  OutputRetryLimit: 'retry_denied_output_retry_limit',
  ResultNotRetained: 'retry_denied_result_not_retained',
  VariablesUnchanged: 'retry_denied_variables_unchanged',
};

// Two of the failures a repeat would meet again have a cause the reader can act on, so they get their own sentence.
const SAME_FAILURE_BY_CODE = {
  TOKEN_QUOTA_EXCEEDED: 'retry_denied_quota',
  TOOL_PERMISSION_DENIED: 'retry_denied_permission',
};

// Applying a saved result again is not running the step in anyone's name, so these three say what is actually asked.
export const OUTPUT_REASON_KEYS = {
  ViewOnly: 'retry_denied_view_only_output',
  InitiatorNotOwner: 'retry_denied_initiator_not_owner_output',
  InitiatorUnavailable: 'retry_denied_initiator_unavailable_output',
};

export const RETRY_REASONS = Object.keys(REASON_KEYS);

/** The retry button is shown only for a task incident the server offers to this reader. */
export function retryOffered(incident) {
  return incident.canRetry === true && Boolean(incident.jobId);
}

/** Applying an accepted result again is a different act from running the step again, and the words say so. */
export function retryWords(incident) {
  return incident.retry?.kind === 'OutputMapping'
    ? { label: text('retry_output'), hint: text('retry_output_hint'), started: text('retry_output_started') }
    : { label: text('retry'), hint: text('retry_hint'), started: text('retry_started') };
}

/**
 * Why this reader is not offered a retry, as a sentence in their language; null
 * when it is offered, or when the server gave no reason (an old server, a
 * resolved incident, a code nobody classified).
 */
export function retryRefusal(incident) {
  if (retryOffered(incident)) return null;
  const retry = incident.retry;
  if (retry?.deniedReason === 'SameFailureRepeats' && SAME_FAILURE_BY_CODE[incident.code]) return text(SAME_FAILURE_BY_CODE[incident.code]);
  if (retry?.deniedReason) {
    const keys = retry.kind === 'OutputMapping' ? { ...REASON_KEYS, ...OUTPUT_REASON_KEYS } : REASON_KEYS;
    return text(keys[retry.deniedReason] ?? 'retry_denied_generic');
  }
  return null;
}

/**
 * What the retry policy has done to this incident, in one sentence: the repeat
 * that is waiting (its number among the allowed ones and how long until it
 * starts) or, once none will start by itself, how many were made. Null for a
 * step without a policy.
 */
export function retryAttemptsText(incident, nowMs = Date.now()) {
  const attempts = incident.attempts;
  if (!attempts) return null;
  if (attempts.nextAttemptAtMs != null) {
    return text('attempts_pending', { number: attempts.made + 1, limit: attempts.limit,
      wait: fmtDuration(Math.max(0, attempts.nextAttemptAtMs - nowMs)) });
  }
  return text('attempts_ended', { made: attempts.made, limit: attempts.limit });
}
