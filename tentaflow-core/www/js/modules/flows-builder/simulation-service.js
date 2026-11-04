// ============ File: simulation-service.js — Service steps of a private simulation: the steps, the answer form, the verdict ============

import { processVerificationMode } from './bpmn.js';

const CODE_PATTERN = /^[A-Za-z0-9_.:-]{1,64}$/;
const SUMMARY_MAX_BYTES = 32 * 1024;
// The engine keeps an activity output to MAX_VARIABLE_BYTES (256 KiB, 128 keys) and a whole
// result to 380 KiB; the browser stops a little short of the first, which also keeps the frame
// far below the transport's 16 MiB and the 1 MiB the socket carried in practice.
export const OUTPUTS_MAX_KIB = 240;
export const OUTPUTS_MAX_KEYS = 128;
const OUTPUTS_MAX_BYTES = OUTPUTS_MAX_KIB * 1024;
const TERMINAL_STATUSES = ['Completed', 'Cancelled', 'Error', 'Incident'];

// Event kinds, run states and task kinds that have a sentence of their own; anything
// else is shown under the name the engine gave it.
export const SIMULATION_EVENT_KINDS = ['instance_started', 'instance_completed', 'instance_error', 'node_completed',
  'script_completed', 'script_result_evaluated', 'exclusive_selected', 'parallel_split', 'parallel_joined',
  'inclusive_split', 'inclusive_joined', 'service_queued', 'service_claimed', 'service_result', 'user_task_opened',
  'user_task_completed', 'user_result_accepted', 'manual_task_opened', 'manual_task_acknowledged',
  'manual_result_accepted', 'verification_created', 'verification_passed', 'verification_approved',
  'verification_rejected', 'verification_withdrawn', 'timer_armed', 'timer_fired', 'timer_cancelled', 'timer_error',
  'end_reached', 'error_end_reached', 'terminate_end_reached', 'incident', 'incident_resolved', 'job_retried',
  'business_error_caught', 'boundary_error', 'boundary_escalation', 'escalation_caught', 'escalation_boundary_armed'];
export const SIMULATION_STATUSES = ['Running', 'Waiting', 'Completed', 'Incident', 'Cancelled', 'Error'];
export const SIMULATION_TASK_KINDS = ['Work', 'Verification', 'Manual'];

export const SERVICE_OUTCOMES = ['Completed', 'Error', 'NeedsHuman', 'Cancelled'];
export const SERVICE_VERDICTS = ['Approved', 'Rejected', 'Unavailable'];

// The Service steps the simulation can run: the engine refuses a step bound to
// an agent or without an implementation, and one with activity IO, when the
// run is created, so none of them is offered a fixture here.
export function simulationServiceSteps(model) {
  return (model?.nodes || []).filter((node) => node.kind?.ServiceTask).map((node) => {
    const task = node.kind.ServiceTask;
    const implementation = task.implementation;
    return {
      nodeId: node.id,
      hasResultExpression: typeof task.resultExpression === 'string' && task.resultExpression.length > 0,
      machineVerdict: ['Test', 'Critic'].includes(processVerificationMode(task.verification)),
      supported: !implementation?.Agent && !implementation?.Unbound && node.activityIo == null,
    };
  }).filter((step) => step.supported);
}

// Without a result expression the engine accepts only a result that completes
// or fails; it is the expression that can turn a result into a request for a
// person or a cancellation.
export function simulationServiceOutcomes(step) {
  return step?.hasResultExpression ? SERVICE_OUTCOMES : SERVICE_OUTCOMES.slice(0, 2);
}

/**
 * Reads a JSON text that has to be an object within the engine's limits. Returns
 * `{ value }` or `{ error }`, the error being an i18n key (under `bpmn.`) or
 * `{ key, values }`.
 */
export function readOutputs(raw) {
  const textValue = String(raw ?? '');
  const bytes = new TextEncoder().encode(textValue).length;
  const tooLarge = { key: 'simulation_outputs_too_large', values: { max: OUTPUTS_MAX_KIB } };
  if (bytes > OUTPUTS_MAX_BYTES * 4) return { error: tooLarge };
  let parsed;
  try {
    parsed = JSON.parse(textValue);
  } catch (_) {
    return { error: 'json_invalid' };
  }
  if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') return { error: 'object_required' };
  if (new TextEncoder().encode(JSON.stringify(parsed)).length > OUTPUTS_MAX_BYTES) return { error: tooLarge };
  if (Object.keys(parsed).length > OUTPUTS_MAX_KEYS) {
    return { error: { key: 'simulation_outputs_too_many_keys', values: { max: OUTPUTS_MAX_KEYS } } };
  }
  return { value: parsed };
}

/**
 * Reads the form of one Service answer. `draft` is `{ outcome, code, summary,
 * outputs (JSON text), verdict }`, `step` is `{ hasResultExpression,
 * machineVerdict }`. Returns `{ result, verdict }` or `{ errors }` with the
 * i18n key (under `bpmn.`) of each field that cannot be sent.
 */
export function readServiceAnswer(step, draft) {
  const errors = {};
  const outcome = draft.outcome;
  if (!simulationServiceOutcomes(step).includes(outcome)) errors.outcome = 'simulation_outcome_unavailable';
  const code = String(draft.code ?? '').trim();
  if (code !== '' && !CODE_PATTERN.test(code)) errors.code = 'simulation_code_invalid';
  const summary = String(draft.summary ?? '');
  if (new TextEncoder().encode(summary).length > SUMMARY_MAX_BYTES) errors.summary = 'simulation_summary_too_long';
  const read = readOutputs(draft.outputs);
  const outputs = read.value ?? null;
  if (read.error) errors.outputs = read.error;
  // A step with a result expression decides its own outcome, so the engine
  // alone knows whether a verdict is needed; without one a result that
  // completes needs it and any other result never reaches the check.
  const verdictNeeded = step.machineVerdict && !step.hasResultExpression && outcome === 'Completed';
  const verdictSent = step.machineVerdict && (step.hasResultExpression ? !!draft.verdict : outcome === 'Completed');
  if (verdictNeeded && !SERVICE_VERDICTS.includes(draft.verdict)) errors.verdict = 'simulation_verdict_required';
  if (Object.keys(errors).length) return { errors };
  return {
    result: { outcome, code: code === '' ? null : code, summary, outputs, evidence: [] },
    verdict: verdictSent ? draft.verdict : null,
  };
}

/**
 * The prepared results chosen in the start window. `choices` maps a node id to
 * `{ mode, draft }`; a step left on `Result` sends nothing, because waiting for
 * the operator is what the engine does without a fixture.
 */
export function readServiceFixtures(steps, choices) {
  const fixtures = [];
  const errors = new Map();
  for (const step of steps) {
    const choice = choices.get(step.nodeId);
    if (choice?.mode !== 'Fixture') continue;
    const answer = readServiceAnswer(step, choice.draft);
    if (answer.errors) {
      errors.set(step.nodeId, answer.errors);
      continue;
    }
    fixtures.push({
      nodeId: step.nodeId, mode: 'Fixture', result: answer.result,
      ...(answer.verdict ? { verdict: answer.verdict } : {}),
    });
  }
  return { fixtures, errors };
}

export function blankServiceDraft() {
  return { outcome: 'Completed', code: '', summary: '', outputs: '{}', verdict: '' };
}

// The steps the view lists as waiting, with what the model knows about each.
export function simulationWaitSteps(waits, model) {
  const known = new Map(simulationServiceSteps(model).map((step) => [step.nodeId, step]));
  return (waits || []).map((wait) => ({
    wait,
    step: { nodeId: wait.nodeId, hasResultExpression: known.get(wait.nodeId)?.hasResultExpression ?? false,
      machineVerdict: wait.verdictApplicable === true },
  }));
}

export function simulationFinished(status) {
  return TERMINAL_STATUSES.includes(status);
}

// A run that is only waiting for an answer is still worth keeping; a finished one is not.
export function simulationOpen(view) {
  const status = view?.instance?.status;
  return !status || status === 'Running' || status === 'Waiting';
}

// The steps the engine refuses when the run is created, with the reason, so the start
// window can say why they get no result source.
export function simulationUnsupportedSteps(model) {
  return (model?.nodes || []).filter((node) => node.kind?.ServiceTask).flatMap((node) => {
    const implementation = node.kind.ServiceTask.implementation;
    const reason = implementation?.Agent ? 'agent' : implementation?.Unbound ? 'unbound' : node.activityIo != null ? 'io' : null;
    return reason ? [{ nodeId: node.id, reason }] : [];
  });
}

// What the view says about a finished task: the verification decision comes from the
// events, because the task itself only keeps its outputs.
export function simulationTaskDecision(events, userTaskId) {
  const event = (events || []).find((candidate) => candidate.data?.user_task_id === userTaskId
    && ['verification_approved', 'verification_rejected'].includes(candidate.kind));
  return event ? (event.kind === 'verification_approved' ? 'approved' : 'rejected') : null;
}
