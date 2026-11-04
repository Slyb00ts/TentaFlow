// ============ File: simulation-service.js — Service steps of a private simulation: the steps, the answer form, the verdict ============

import { processVerificationMode } from './bpmn.js';

const CODE_PATTERN = /^[A-Za-z0-9_.:-]{1,64}$/;
const SUMMARY_MAX_BYTES = 32 * 1024;

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
  let outputs = null;
  try {
    outputs = JSON.parse(String(draft.outputs ?? ''));
    if (!outputs || Array.isArray(outputs) || typeof outputs !== 'object') errors.outputs = 'object_required';
  } catch (_) {
    errors.outputs = 'json_invalid';
  }
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
