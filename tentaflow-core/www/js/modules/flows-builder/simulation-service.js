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
  'business_error_caught', 'boundary_error', 'boundary_escalation', 'escalation_caught', 'escalation_boundary_armed',
  'simulation_message_sent', 'message_queued', 'message_delivered', 'message_ambiguous', 'message_blocked',
  'message_cancelled', 'message_error', 'message_expired', 'message_resolved', 'receive_task_opened',
  'receive_task_completed', 'send_task_admitted', 'send_task_pending', 'send_admission_failed', 'signal_admitted',
  'signal_catch_opened', 'signal_received', 'event_race_armed', 'event_race_won', 'event_race_lost',
  'event_race_cancelled', 'subscription_cancelled', 'timer_blocked', 'link_thrown', 'link_caught', 'message_armed',
  'error_boundary_armed', 'cancelled', 'job_failed', 'job_denied', 'simulation_service_cancelled', 'simulation_user_task_cancelled',
  'scope_entered', 'scope_completed', 'scope_cancelled', 'scope_entry_failed', 'scope_error_propagated', 'scope_result_accepted',
  'call_requested', 'call_entered', 'call_returned', 'call_cancelled', 'call_error_propagated', 'call_result_accepted',
  'call_request_cancelled', 'repetition_group_started', 'repetition_occurrence_started', 'repetition_occurrence_completed',
  'repetition_completed', 'repetition_condition_checked', 'repetition_entry_failed', 'repetition_group_blocked', 'instance_cancelled'];
export const SIMULATION_MESSAGE_STATUSES = ['Pending', 'Blocked', 'Ambiguous', 'Delivered', 'Expired', 'Cancelled', 'Error'];
export const SIMULATION_SUBSCRIPTION_KINDS = ['MessageCatch', 'BoundaryMessage', 'ReceiveTask', 'SignalCatch', 'BoundaryError', 'BoundaryEscalation'];
const MESSAGE_WAIT_KINDS = ['MessageCatch', 'BoundaryMessage', 'ReceiveTask'];
const MESSAGE_KEY_MAX_BYTES = 256;
export const MESSAGE_TTL_DEFAULT = 3600;
const MESSAGE_TTL_MAX = 604800;
// The incident codes a message or a signal can leave behind, each with a sentence of its own.
export const SIMULATION_EVENT_INCIDENT_CODES = ['CALL_ADMISSION_ERROR', 'CALL_CHILD_CANCELLED', 'CALL_CHILD_ERROR', 'CALL_RETURN_ERROR',
  'SCOPE_RETURN_ERROR', 'MESSAGE_MAPPING_ERROR', 'MESSAGE_PREDICATE_ERROR', 'SIGNAL_DELIVERY_FAILED',
  'SIGNAL_MAPPING_FAILED', 'SIGNAL_PAYLOAD_FAILED', 'SIGNAL_PAYLOAD_LIMIT', 'SIGNAL_PENDING_LIMIT', 'SIGNAL_RECIPIENT_LIMIT'];
export const SIMULATION_STATUSES = ['Running', 'Waiting', 'Completed', 'Incident', 'Cancelled', 'Error'];
export const SIMULATION_TASK_KINDS = ['Work', 'Verification', 'Manual'];

export const SERVICE_OUTCOMES = ['Completed', 'Error', 'NeedsHuman', 'Cancelled'];
export const SERVICE_VERDICTS = ['Approved', 'Rejected', 'Unavailable'];

// The Service steps the simulation can run: the engine refuses a step bound to
// an agent or without an implementation, and one with activity IO, when the
// run is created, so none of them is offered a fixture here.
// The nodes of the simulated process: its own and those inside its sub-processes, which run in
// scopes of the same instance. Processes it calls are other documents' nodes.
function ownNodes(model) {
  const found = [];
  const visit = (body) => {
    for (const node of body?.nodes || []) {
      found.push(node);
      if (node.kind?.SubProcess) visit(node.kind.SubProcess.body);
    }
  };
  visit(model);
  return found;
}

export function simulationServiceSteps(model) {
  return ownNodes(model).filter((node) => node.kind?.ServiceTask).map((node) => {
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

// Prepared results are keyed by the nodes of the simulated process; a process it calls has steps of
// its own, which wait for the operator.
export function simulationCallsOtherProcesses(model) {
  return ownNodes(model).some((node) => node.kind?.CallActivity);
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
export function readOutputs(raw, kind = 'outputs') {
  const textValue = String(raw ?? '');
  const bytes = new TextEncoder().encode(textValue).length;
  const tooLarge = { key: `simulation_${kind}_too_large`, values: { max: OUTPUTS_MAX_KIB } };
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
    return { error: { key: `simulation_${kind}_too_many_keys`, values: { max: OUTPUTS_MAX_KEYS } } };
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

// The steps the view lists as waiting. A step of a called process can share its node id with one
// of the model, so what the wait itself carries wins over a lookup of the model by node id.
export function simulationWaitSteps(waits, model) {
  const known = new Map(simulationServiceSteps(model).map((step) => [step.nodeId, step]));
  return (waits || []).map((wait) => ({
    wait,
    step: { nodeId: wait.nodeId,
      hasResultExpression: wait.hasResultExpression ?? known.get(wait.nodeId)?.hasResultExpression ?? false,
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
  const eventWithIo = ['MessageCatch', 'MessageThrow', 'ReceiveTask', 'SendTask', 'SignalThrow', 'SignalCatch'];
  return ownNodes(model).flatMap((node) => {
    const kind = typeof node.kind === 'object' ? Object.keys(node.kind)[0] : node.kind;
    if (kind === 'ServiceTask') {
      const implementation = node.kind.ServiceTask.implementation;
      const reason = implementation?.Agent ? 'agent' : implementation?.Unbound ? 'unbound' : node.activityIo != null ? 'io' : null;
      return reason ? [{ nodeId: node.id, reason }] : [];
    }
    if (kind === 'CallActivity' && node.activityIo != null) return [{ nodeId: node.id, reason: 'call_io' }];
    if (kind === 'MessageStart') return [{ nodeId: node.id, reason: 'message_start' }];
    if (eventWithIo.includes(kind) && node.activityIo != null) return [{ nodeId: node.id, reason: 'event_io' }];
    return [];
  });
}

// What the view says about a finished task: the verification decision comes from the
// events, because the task itself only keeps its outputs.
export function simulationTaskDecision(events, userTaskId) {
  const event = (events || []).find((candidate) => candidate.data?.user_task_id === userTaskId
    && ['verification_approved', 'verification_rejected'].includes(candidate.kind));
  return event ? (event.kind === 'verification_approved' ? 'approved' : 'rejected') : null;
}

// The open waits an operator message can address: the ones that take a message.
export function simulationMessageWaits(subscriptions) {
  return (subscriptions || []).filter((subscription) => MESSAGE_WAIT_KINDS.includes(subscription.kind));
}

export function blankMessageDraft() {
  return { pick: '', name: '', key: '', payload: '{}', ttl: String(MESSAGE_TTL_DEFAULT), exact: false };
}

const validKey = (value) => {
  const bytes = new TextEncoder().encode(value).length;
  return bytes > 0 && bytes <= MESSAGE_KEY_MAX_BYTES && !/[\u0000-\u001f\u007f-\u009f]/.test(value);
};

/**
 * Reads the message form into the request fields, or names each wrong field with an i18n
 * key under `bpmn.`. The engine judges the same rules (a literal non-empty name and key of at
 * most 256 bytes, a payload object within the output limits, a lifetime of 1..604800 seconds);
 * `subscriptions` are the waits of the view, against which a chosen wait is checked.
 */
export function readMessageDraft(draft, subscriptions) {
  const errors = {};
  const name = String(draft.name ?? '');
  const key = String(draft.key ?? '');
  if (!validKey(name)) errors.name = 'simulation_message_name_invalid';
  if (!validKey(key)) errors.key = 'simulation_message_key_invalid';
  const ttl = String(draft.ttl ?? '').trim();
  const ttlSeconds = /^\d+$/.test(ttl) ? Number(ttl) : NaN;
  if (!(ttlSeconds >= 1 && ttlSeconds <= MESSAGE_TTL_MAX)) errors.ttl = 'simulation_message_ttl_invalid';
  const payload = readOutputs(draft.payload, 'payload');
  if (payload.error) errors.payload = payload.error;
  let subscriptionId = null;
  if (draft.exact && draft.pick) {
    const chosen = simulationMessageWaits(subscriptions).find((subscription) => subscription.subscriptionId === draft.pick);
    if (!chosen) errors.pick = 'simulation_message_pick_gone';
    else if (chosen.messageName !== name || chosen.correlationKey !== key) errors.pick = 'simulation_message_pick_mismatch';
    else subscriptionId = chosen.subscriptionId;
  }
  if (Object.keys(errors).length) return { errors };
  return { request: { messageName: name, correlationKey: key, payload: payload.value, ttlSeconds, subscriptionId } };
}

// The open message waits a message would reach by its name and key.
export function simulationMessageCandidates(subscriptions, message) {
  return simulationMessageWaits(subscriptions).filter((subscription) => subscription.messageName === message.messageName
    && subscription.correlationKey === message.correlationKey);
}

// The clock stands on the end of the simulation: there is nothing to advance to.
export function simulationHorizonReached(clock) {
  return clock != null && clock.nowMs >= clock.horizonMs;
}

// The instance every scope belongs to, as the events say it (the view's scopes do not name one).
export function simulationScopeInstances(view) {
  const owner = new Map();
  for (const event of view?.events || []) if (event.scopeId && event.instanceId) owner.set(event.scopeId, event.instanceId);
  return owner;
}

/**
 * The call tree of a run for display: each instance (parents before children, as the view lists them)
 * with the call that started it, the sub-process scopes it opened and the repetition groups it holds.
 * A scope or group whose instance the events do not tell belongs to the simulated instance.
 */
export function simulationStructure(view) {
  const instances = view?.instances || [];
  if (!instances.length) return [];
  const root = instances[0].instanceId;
  const owner = simulationScopeInstances(view);
  const instanceOf = (scopeId) => owner.get(scopeId) ?? root;
  const occurrenceOf = (instance) => {
    const entered = (view.events || []).find((event) => event.kind === 'call_entered' && event.data?.child_instance_id === instance.instanceId);
    const tokenId = entered?.data?.parent_token_id;
    return tokenId ? (view.repetitionOccurrences || []).find((occurrence) => occurrence.tokenId === tokenId) ?? null : null;
  };
  return instances.map((instance) => ({
    instance,
    depth: instance.depth ?? 0,
    occurrence: occurrenceOf(instance),
    call: (view.calls || []).find((call) => call.childInstanceId === instance.instanceId) ?? null,
    scopes: (view.scopes || []).filter((scope) => scope.subprocessNodeId != null && instanceOf(scope.scopeId) === instance.instanceId),
    groups: (view.repetitionGroups || []).filter((group) => instanceOf(group.scopeId) === instance.instanceId).map((group) => ({
      group,
      occurrences: (view.repetitionOccurrences || []).filter((occurrence) => occurrence.groupId === group.groupId),
    })),
  }));
}

// A run with more than one instance names the instance on what belongs to one.
export function simulationInstanceLabel(instance) {
  return instance ? `${instance.definitionName} v${instance.version}` : '';
}

/**
 * The tasks with the repetition occurrence each belongs to, in reading order: tasks of one
 * repetition group stay together, by occurrence ordinal, whatever order the engine listed them in.
 */
export function simulationTasksByOccurrence(tasks, occurrences) {
  const byTask = new Map((occurrences || []).filter((occurrence) => occurrence.userTaskId || occurrence.verificationUserTaskId)
    .flatMap((occurrence) => [[occurrence.userTaskId, occurrence], [occurrence.verificationUserTaskId, occurrence]]
      .filter(([id]) => id)));
  const firstOfGroup = new Map();
  const rows = (tasks || []).map((task, index) => {
    const occurrence = byTask.get(task.userTaskId) ?? null;
    if (occurrence && !firstOfGroup.has(occurrence.groupId)) firstOfGroup.set(occurrence.groupId, index);
    return { task, occurrence, index };
  });
  const place = (row) => (row.occurrence ? firstOfGroup.get(row.occurrence.groupId) : row.index);
  return rows.sort((a, b) => place(a) - place(b) || (a.occurrence?.ordinal ?? 0) - (b.occurrence?.ordinal ?? 0) || a.index - b.index);
}
