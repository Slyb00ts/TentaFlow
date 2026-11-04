// ============ File: process-simulation.js — deterministic private process simulation UI ============

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { TfWindow } from '/js/components/tf-window.js';
import { processDistinctLabels, processElementNames, processExplainError, processRepeatedElements, processStartEntryLabels } from './bpmn.js';
import { processIncidentText } from './process-monitor.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-input.js';
import { answerFormHtml, errorMessage, fillAnswerForm, readAnswerDraft, showAnswerErrors, syncAnswerForm } from './simulation-answer-form.js';
import {
  OUTPUTS_MAX_KIB, SIMULATION_EVENT_INCIDENT_CODES, SIMULATION_EVENT_KINDS, SIMULATION_MESSAGE_STATUSES,
  SIMULATION_STATUSES, SIMULATION_SUBSCRIPTION_KINDS, SIMULATION_TASK_KINDS, blankMessageDraft, blankServiceDraft,
  readMessageDraft, readOutputs, readServiceAnswer, readServiceFixtures, simulationFinished, simulationMessageWaits,
  simulationOpen, simulationServiceSteps, simulationTaskDecision, simulationUnsupportedSteps, simulationWaitSteps,
} from './simulation-service.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-select.js';

const SIMULATION_EVENTS_SHOWN = 50;

const text = (key, values) => I18n.t(`bpmn.${key}`, values);

function jsonSection(label, value) {
  const section = document.createElement('section');
  section.className = 'fb-process-json';
  section.innerHTML = `<h3>${escapeHtml(label)}</h3><tf-code-editor language="json" aria-label="${escapeAttr(label)}"></tf-code-editor><tf-alert data-json-error tone="danger" hidden></tf-alert>`;
  const editor = section.querySelector('tf-code-editor');
  editor.value = JSON.stringify(value ?? {}, null, 2);
  section.readValue = () => {
    try {
      const parsed = JSON.parse(editor.value);
      if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') {
        throw new Error(text('object_required'));
      }
      section.querySelector('[data-json-error]').hidden = true;
      return parsed;
    } catch (error) {
      const message = error instanceof SyntaxError ? text('json_invalid') : error.message;
      const errorEl = section.querySelector('[data-json-error]');
      errorEl.setAttribute('message', message);
      errorEl.hidden = false;
      throw error;
    }
  };
  return section;
}

const MAPPING_CODES = ['TRANSITION_ERROR', 'RESULT_REJECTED', 'RESULT_ACCEPTANCE_FAILED'];

// A run kept across a language switch, which remounts the screen and closes every overlay.
let carried = null;
// The runs whose window is up. The router closes a stray window with an animation, so its
// `closed` can arrive after the new screen has already asked for the run back.
const live = new Set();

const known = (list, value, prefix) => (list.includes(value) ? text(`${prefix}_${value.toLowerCase()}`) : String(value ?? ''));

// An incident in words: the platform's sentence for the codes it knows, the cause of the
// common ones, and for a code the step reported itself, that code.
export function simulationIncidentText(incident) {
  const code = String(incident.code || '');
  if (code === 'RESULT_EXPRESSION_ERROR') return text('simulation_incident_result_expression');
  if (SIMULATION_EVENT_INCIDENT_CODES.includes(code)) return text(`simulation_incident_${code.toLowerCase()}`);
  const sentence = processIncidentText({ code, message: '' });
  if (sentence === text('incident_generic') && code !== '') {
    return incident.message || text('simulation_incident_service_code', { code });
  }
  return MAPPING_CODES.includes(code) ? `${sentence} ${text('simulation_incident_mapping_hint')}` : sentence;
}

// The refusals of a run that moved on while the window was open, then the model-aware ones.
export function simulationErrorText(message, model) {
  const raw = String(message ?? '');
  if (/message too big/.test(raw)) return text('simulation_error_too_big', { max: OUTPUTS_MAX_KIB });
  if (/target instance is closed/.test(raw)) return text('simulation_error_target_closed');
  if (/revision conflict or closed instance/.test(raw)) return text('simulation_error_closed');
  if (/simulation task is assigned to another actor|simulation user task was not found/.test(raw)) return text('simulation_error_task_unavailable');
  if (/process resource not found/.test(raw)) return text('simulation_error_gone');
  return model ? processExplainError(raw, model) : raw;
}

const clockText = (ms) => new Date(ms).toLocaleString(I18n.getLanguage());
const timeText = (ms) => new Date(ms).toLocaleTimeString(I18n.getLanguage());

export function simulationWindow(view, model = null, options = {}) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', text('simulation'));
  win.setAttribute('icon', 'play');
  win.setAttribute('modal', '');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '940');
  win.setAttribute('min-width', '420');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.classList.add('fb-process-window');
  win.innerHTML = '<div slot="body" class="fb-process-content"><tf-alert data-error tone="danger" hidden></tf-alert><div data-simulation-content></div></div>';
  document.body.appendChild(win);
  const content = win.querySelector('[data-simulation-content]');
  const language = I18n.getLanguage();
  const entry = { language, handedOver: false, snapshot: null };
  let current = view;
  let busy = false;
  let ending = false;
  // The prepared results chosen at start: the view does not say where a result came from.
  const fixtures = options.fixtures ?? new Map();
  // The run reports elements by id; the author knows them by the names of the
  // model that was started.
  const elementNames = model ? processElementNames(model) : new Map();
  const elementName = (nodeId, name) => String(name || '').trim() || elementNames.get(nodeId) || text('element_unavailable');
  // What the author typed survives the next re-render, which rebuilds every control.
  const drafts = new Map(options.drafts ?? []);
  const taskDrafts = new Map(options.taskDrafts ?? []);
  // Errors for a field that only the server could judge, shown once after the redraw.
  const fieldErrors = new Map();
  // The message form is one form for the whole run, so it has one draft.
  let messageDraft = { ...blankMessageDraft(), ...(options.messageDraft ?? {}) };

  const setError = (error) => {
    const alert = win.querySelector('[data-error]');
    const message = error?.message || String(error);
    alert.setAttribute('message', simulationErrorText(message, model));
    alert.hidden = false;
    alert.scrollIntoView?.({ block: 'nearest' });
  };

  const run = async (action, onFail = null) => {
    if (busy || !win.isConnected) return;
    busy = true;
    win.querySelectorAll('tf-button').forEach((button) => button.setAttribute('disabled', ''));
    win.querySelector('[data-error]').hidden = true;
    try {
      current = await action();
      render();
    } catch (error) {
      setError(error);
      onFail?.(error);
    } finally {
      busy = false;
      if (win.isConnected) render();
    }
  };

  const messageField = (name) => content.querySelector(`[data-message-field="${name}"]`);
  const readMessageForm = () => ({
    pick: messageField('pick')?.value ?? '', name: messageField('name')?.value ?? '', key: messageField('key')?.value ?? '',
    payload: messageField('payload')?.value ?? '', ttl: messageField('ttl')?.value ?? '', exact: messageField('exact')?.checked === true,
  });

  const captureDrafts = () => {
    if (messageField('name')) messageDraft = readMessageForm();
    content.querySelectorAll('[data-simulation-wait]').forEach((article) => {
      if (article.querySelector('[data-answer-field]')) drafts.set(article.dataset.simulationWait, readAnswerDraft(article));
    });
    content.querySelectorAll('[data-simulation-output]').forEach((editor) => {
      taskDrafts.set(editor.dataset.simulationOutput, editor.value);
    });
  };

  const render = () => {
    if (!win.isConnected) return;
    captureDrafts();
    const waits = simulationWaitSteps(current.serviceWaits, model);
    for (const jobId of [...drafts.keys()]) {
      if (!waits.some(({ wait }) => wait.jobId === jobId)) drafts.delete(jobId);
    }
    const allTasks = current.userTasks || [];
    for (const taskId of [...taskDrafts.keys()]) {
      if (!allTasks.some((task) => task.userTaskId === taskId && task.status === 'Open')) taskDrafts.delete(taskId);
    }
    const clock = current.clock;
    const status = current.instance?.status || 'Running';
    const openTasks = allTasks.filter((task) => task.status === 'Open');
    const doneTasks = allTasks.filter((task) => task.status !== 'Open');
    const timers = current.timers || [];
    const events = current.events || [];
    const incidents = current.incidents || [];
    const shownEvents = events.slice(-SIMULATION_EVENTS_SHOWN);
    // The server omits what does not fit one frame and this view shows only the
    // newest rows of the rest, so the label counts both kinds of hidden events.
    const hiddenEvents = (current.eventsOmitted || 0) + events.length - shownEvents.length;
    const finished = simulationFinished(status);
    const eventLabel = (event) => {
      if (event.kind === 'service_result' && fixtures.has(event.nodeId)) return text('simulation_event_service_result_fixture');
      return known(SIMULATION_EVENT_KINDS, event.kind, 'simulation_event');
    };
    const decision = (task) => simulationTaskDecision(events, task.userTaskId);
    const subscriptions = current.subscriptions || [];
    const messages = current.messages || [];
    const messageWaits = simulationMessageWaits(subscriptions);
    const waitLabel = (subscription) => `${elementName(subscription.nodeId, subscription.nodeName)} · ${subscription.messageName} · ${subscription.correlationKey}`;
    const subscriptionRow = (subscription) => `<li data-simulation-subscription="${escapeAttr(subscription.subscriptionId)}" data-subscription-kind="${escapeAttr(subscription.kind)}">
        <strong>${escapeHtml(elementName(subscription.nodeId, subscription.nodeName))}</strong> · ${escapeHtml(known(SIMULATION_SUBSCRIPTION_KINDS, subscription.kind, 'simulation_subscription_kind'))}
        ${subscription.messageName != null ? `<div>${escapeHtml(text('simulation_sub_message'))}: ${escapeHtml(subscription.messageName)} · ${escapeHtml(text('simulation_sub_key'))}: ${escapeHtml(subscription.correlationKey ?? '')}</div>` : ''}
        ${subscription.kind === 'SignalCatch' ? `<div>${escapeHtml(text('simulation_sub_signal'))}: ${escapeHtml(subscription.signalDeclarationId || subscription.signalNamespaceUri || '')}</div>` : ''}
        ${subscription.dueAtMs != null ? `<div>${escapeHtml(text('simulation_sub_timer_due', { time: clockText(subscription.dueAtMs) }))}</div>` : ''}
      </li>`;
    const races = new Map();
    for (const subscription of subscriptions) {
      if (subscription.raceId) races.set(subscription.raceId, [...(races.get(subscription.raceId) ?? []), subscription]);
    }
    const messageRow = (message) => {
      const state = message.status;
      const when = state === 'Delivered' && message.deliveredAtMs != null ? text('simulation_message_delivered_at', { time: clockText(message.deliveredAtMs) })
        : state === 'Expired' ? text('simulation_message_expired_at', { time: clockText(message.expiresAtMs) })
          : ['Pending', 'Ambiguous', 'Blocked'].includes(state) ? text('simulation_message_expires_at', { time: clockText(message.expiresAtMs) }) : '';
      return `<li data-simulation-message="${escapeAttr(message.messageId)}" data-message-status="${escapeAttr(state)}">
        <strong>${escapeHtml(message.messageName)}</strong> · ${escapeHtml(text('simulation_sub_key'))}: ${escapeHtml(message.correlationKey)}
        <tf-chip status="${state === 'Delivered' ? 'ok' : state === 'Error' ? 'err' : state === 'Pending' ? 'info' : 'warn'}">${escapeHtml(known(SIMULATION_MESSAGE_STATUSES, state, 'simulation_message_status'))}</tf-chip>
        ${when ? `<div>${escapeHtml(when)}</div>` : ''}
        ${state === 'Pending' ? `<div class="fb-field-hint">${escapeHtml(text('simulation_message_pending_hint'))}</div>` : ''}
        ${state === 'Ambiguous' ? `<div class="fb-field-hint" data-message-ambiguous-hint>${escapeHtml(text('simulation_message_ambiguous_hint'))}</div>
          <tf-button variant="secondary" size="sm" data-message-address="${escapeAttr(message.messageId)}" ${busy || finished ? 'disabled' : ''}>${escapeHtml(text('simulation_message_address'))}</tf-button>` : ''}
        ${['Error', 'Blocked'].includes(state) && message.lastReason ? `<div class="fb-field-hint">${escapeHtml(message.lastReason)}</div>` : ''}
      </li>`;
    };
    const lock = busy || finished ? 'disabled' : '';
    content.innerHTML = `
      <div class="fb-simulation-toolbar">
        <div><strong>${escapeHtml(text('simulation_clock'))}</strong>: ${escapeHtml(text('simulation_clock_value', { now: clockText(clock.nowMs), end: clockText(clock.horizonMs), step: clock.stepIndex }))}</div>
        <div><span>${escapeHtml(text('status'))}: <tf-chip status="${status === 'Incident' || status === 'Error' ? 'err' : status === 'Completed' ? 'ok' : 'info'}" data-simulation-status="${escapeAttr(status)}">${escapeHtml(known(SIMULATION_STATUSES, status, 'simulation_status'))}</tf-chip></span>
          <tf-button variant="secondary" size="sm" icon="plus" data-simulation-advance ${busy || finished ? 'disabled' : ''}>${escapeHtml(text('simulation_advance'))}</tf-button>
          <tf-button variant="ghost" size="sm" data-simulation-end ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_end'))}</tf-button></div>
      </div>
      <p class="fb-field-hint" data-simulation-advance-hint>${escapeHtml(text('simulation_advance_hint'))}</p>
      ${incidents.length || status === 'Incident' ? `<section class="fb-simulation-section" data-simulation-incidents><h3>${escapeHtml(text('simulation_incidents'))}</h3>
        <p>${escapeHtml(text('simulation_incident_stopped'))}</p>
        ${incidents.map((incident) => `<tf-alert tone="danger" data-simulation-incident message="${escapeAttr(`${elementName(incident.nodeId, incident.nodeName)}: ${simulationIncidentText(incident)} (${incident.code})`)}"></tf-alert>`).join('')}
      </section>` : ''}
      ${waits.length ? `<section class="fb-simulation-section" data-simulation-waits><h3>${escapeHtml(text('simulation_waits', { count: waits.length }))}</h3>
        ${waits.map(({ wait, step }) => {
          const prepared = wait.mode === 'Fixture';
          const fixture = prepared ? fixtures.get(wait.nodeId) : null;
          return `<article class="fb-simulation-wait" data-simulation-wait="${escapeAttr(wait.jobId)}" data-node-id="${escapeAttr(wait.nodeId)}">
          <div><strong>${escapeHtml(elementName(wait.nodeId))}</strong>
            <tf-chip status="info">${escapeHtml(text(prepared ? 'simulation_wait_fixture' : 'simulation_wait_result'))}</tf-chip></div>
          <section class="fb-process-json"><h4>${escapeHtml(text('simulation_wait_input'))}</h4>
            <tf-code-editor readonly data-simulation-wait-input language="json" aria-label="${escapeAttr(text('simulation_wait_input'))}"></tf-code-editor></section>
          ${prepared ? `<section class="fb-process-json" data-simulation-prepared><h4>${escapeHtml(text('simulation_prepared_values'))}</h4>
              ${fixture ? `<p>${escapeHtml(text('simulation_outcome'))}: <strong>${escapeHtml(text(`simulation_outcome_${{ Completed: 'completed', Error: 'error', NeedsHuman: 'needs_human', Cancelled: 'cancelled' }[fixture.result.outcome]}`))}</strong>${fixture.result.code ? ` · ${escapeHtml(fixture.result.code)}` : ''}${fixture.verdict ? ` · ${escapeHtml(text(`simulation_verdict_${fixture.verdict.toLowerCase()}`))}` : ''}</p>
              <p>${escapeHtml(text('simulation_result_summary'))}: ${escapeHtml(fixture.result.summary || '—')}</p>
              <tf-code-editor readonly data-simulation-prepared-outputs language="json" aria-label="${escapeAttr(text('simulation_result_outputs'))}"></tf-code-editor>` : ''}
            </section>`
            : `${answerFormHtml(step, drafts.get(wait.jobId) ?? blankServiceDraft())}
          <tf-button variant="primary" size="sm" data-simulation-resolve="${escapeAttr(wait.jobId)}" ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_resolve'))}</tf-button>`}
        </article>`;
        }).join('')}
      </section>` : ''}
      <section class="fb-simulation-section"><h3>${escapeHtml(text('simulation_tasks'))}</h3>
        ${openTasks.length ? openTasks.map((task) => {
          const manual = task.kind === 'Manual';
          const verification = task.kind === 'Verification';
          const id = escapeAttr(task.userTaskId);
          // A verification task has one question, and its answer is the decision.
          const controls = manual
            ? `<tf-button variant="secondary" size="sm" data-simulation-task="${id}" data-simulation-manual="true" ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_acknowledge_task'))}</tf-button>`
            : verification
              ? `<p>${escapeHtml(text('simulation_verification_hint'))}</p>
                <tf-button variant="primary" size="sm" data-simulation-decide="true" data-simulation-task="${id}" data-simulation-manual="false" ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_approve_task'))}</tf-button>
                <tf-button variant="danger" size="sm" data-simulation-decide="false" data-simulation-task="${id}" data-simulation-manual="false" ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_reject_task'))}</tf-button>`
              : `<tf-code-editor data-simulation-output="${id}" language="json" aria-label="${escapeAttr(text('simulation_outputs'))}"></tf-code-editor>
                <p class="fb-field-hint">${escapeHtml(text('simulation_editor_escape_hint'))}</p>
                <tf-alert data-task-error tone="danger" hidden></tf-alert>
                <tf-button variant="primary" size="sm" data-simulation-task="${id}" data-simulation-manual="false" ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_complete_task'))}</tf-button>`;
          return `<article class="fb-simulation-task" data-simulation-open-task="${id}"><div><strong>${escapeHtml(elementName(task.nodeId, task.name))}</strong> · ${escapeHtml(known(SIMULATION_TASK_KINDS, task.kind, 'simulation_task_kind'))}</div>
            <div>${escapeHtml(task.instructions || '')}</div>
            ${controls}
          </article>`;
        }).join('') : `<p>${escapeHtml(text('simulation_empty_tasks'))}</p>`}
        ${doneTasks.length ? `<h3>${escapeHtml(text('simulation_tasks_done'))}</h3>${doneTasks.map((task) => {
          const decided = decision(task);
          return `<article class="fb-simulation-task" data-simulation-done-task="${escapeAttr(task.userTaskId)}"><div><strong>${escapeHtml(elementName(task.nodeId, task.name))}</strong> · ${escapeHtml(known(SIMULATION_TASK_KINDS, task.kind, 'simulation_task_kind'))}
            <tf-chip status="${task.status === 'Completed' ? 'ok' : 'warn'}">${escapeHtml(text(task.status === 'Completed' ? 'simulation_task_completed' : 'simulation_task_cancelled'))}</tf-chip>
            ${decided ? `<tf-chip status="${decided === 'approved' ? 'ok' : 'err'}" data-simulation-decision="${decided}">${escapeHtml(text(decided === 'approved' ? 'simulation_decision_approved' : 'simulation_decision_rejected'))}</tf-chip>` : ''}</div></article>`;
        }).join('')}` : ''}
      </section>
      ${subscriptions.length ? `<section class="fb-simulation-section" data-simulation-awaited><h3>${escapeHtml(text('simulation_awaited'))}</h3>
        <ul class="fb-simulation-awaited">
          ${subscriptions.filter((subscription) => !subscription.raceId).map(subscriptionRow).join('')}
          ${[...races.entries()].map(([raceId, group]) => `<li data-simulation-race="${escapeAttr(raceId)}"><em>${escapeHtml(text('simulation_race', { count: group.length }))}</em>
            <ul>${group.map(subscriptionRow).join('')}</ul></li>`).join('')}
        </ul>
        ${subscriptions.some((subscription) => subscription.kind === 'SignalCatch') ? `<p class="fb-field-hint" data-simulation-signal-note>${escapeHtml(text('simulation_signal_note'))}</p>` : ''}
      </section>` : ''}
      <section class="fb-simulation-section" data-simulation-message-form><h3>${escapeHtml(text('simulation_send_message'))}</h3>
        ${finished ? `<p data-message-finished>${escapeHtml(text('simulation_message_finished'))}</p>` : ''}
        <div class="fb-simulation-answer">
          <tf-select data-message-field="pick" wrap-selected label="${escapeAttr(text('simulation_message_pick'))}" hint="${escapeAttr(text('simulation_message_pick_hint'))}" value="${escapeAttr(messageDraft.pick)}" ${lock}>
            <option value="">${escapeHtml(text('simulation_message_free'))}</option>
            ${messageWaits.map((subscription) => `<option value="${escapeAttr(subscription.subscriptionId)}">${escapeHtml(waitLabel(subscription))}</option>`).join('')}
          </tf-select>
          <tf-checkbox data-message-field="exact" label="${escapeAttr(text('simulation_message_exact'))}" ${messageDraft.exact ? 'checked' : ''} ${lock}></tf-checkbox>
          <tf-input data-message-field="name" label="${escapeAttr(text('simulation_message_name'))}" value="${escapeAttr(messageDraft.name)}" ${lock}></tf-input>
          <tf-alert data-message-error="name" tone="danger" hidden></tf-alert>
          <tf-input data-message-field="key" label="${escapeAttr(text('simulation_message_key'))}" value="${escapeAttr(messageDraft.key)}" ${lock}></tf-input>
          <tf-alert data-message-error="key" tone="danger" hidden></tf-alert>
          <section class="fb-process-json"><h4>${escapeHtml(text('simulation_message_payload'))}</h4>
            <tf-code-editor data-message-field="payload" language="json" ${finished ? 'readonly' : ''} aria-label="${escapeAttr(text('simulation_message_payload'))}"></tf-code-editor>
            <p class="fb-field-hint">${escapeHtml(text('simulation_editor_escape_hint'))}</p>
            <tf-alert data-message-error="payload" tone="danger" hidden></tf-alert></section>
          <tf-input data-message-field="ttl" type="number" min="1" max="604800" label="${escapeAttr(text('simulation_message_ttl'))}" hint="${escapeAttr(text('simulation_message_ttl_hint'))}" value="${escapeAttr(messageDraft.ttl)}" ${lock}></tf-input>
          <tf-alert data-message-error="ttl" tone="danger" hidden></tf-alert>
          <tf-alert data-message-error="pick" tone="danger" hidden></tf-alert>
          <tf-button variant="primary" size="sm" data-simulation-message-send ${lock}>${escapeHtml(text('simulation_message_send'))}</tf-button>
        </div>
      </section>
      ${messages.length ? `<section class="fb-simulation-section" data-simulation-messages><h3>${escapeHtml(text('simulation_messages', { count: messages.length }))}</h3>
        <ul class="fb-simulation-awaited">${messages.map(messageRow).join('')}</ul></section>` : ''}
      <section class="fb-simulation-section"><h3>${escapeHtml(text('simulation_timers'))}</h3>
        ${timers.length ? timers.map((timer) => {
          const timerStatus = String(timer.status || '').toLowerCase();
          const statusKey = ['pending', 'fired', 'blocked', 'cancelled', 'missed', 'error'].includes(timerStatus)
            ? `simulation_timer_${timerStatus}` : null;
          const due = timer.dueAtMs == null ? '—' : clockText(timer.dueAtMs);
          return `<article class="fb-simulation-timer"><div><strong>${escapeHtml(elementName(timer.nodeId, timer.nodeName))}</strong> · ${escapeHtml(known(['Start', 'Catch', 'Boundary'], String(timer.kind || ''), 'simulation_timer_kind'))}${timer.attachedToId ? ` · ${escapeHtml(text('simulation_timer_attached', { node: elementName(timer.attachedToId) }))}` : ''}
            <tf-chip status="info">${escapeHtml(statusKey ? text(statusKey) : String(timer.status || ''))}</tf-chip></div>
            <div>${escapeHtml(text('simulation_timer_due'))}: ${escapeHtml(due)} · ${escapeHtml(text('simulation_timer_occurrence'))}: ${escapeHtml(String(timer.occurrence ?? 0))}</div>
          </article>`;
        }).join('') : `<p>${escapeHtml(text('simulation_empty_timers'))}</p>`}
      </section>
      <section class="fb-simulation-section"><h3>${escapeHtml(text('simulation_events'))}</h3>
        ${events.length ? `<ol class="fb-simulation-events">${shownEvents.map((event) => `<li><span>${escapeHtml(timeText(event.atMs))}</span> ${escapeHtml(eventLabel(event))}${event.nodeId ? ` · ${escapeHtml(elementName(event.nodeId))}` : ''}</li>`).join('')}</ol>` : `<p>${escapeHtml(text('simulation_empty_events'))}</p>`}
        ${hiddenEvents > 0 ? `<p data-simulation-events-omitted>${escapeHtml(text('simulation_events_omitted', { count: hiddenEvents }))}</p>` : ''}
        ${current.traceStepsOmitted > 0 ? `<p data-simulation-trace-omitted>${escapeHtml(text('simulation_trace_steps_omitted', { count: current.traceStepsOmitted }))}</p>` : ''}
      </section>`;
    content.querySelectorAll('tf-code-editor[data-simulation-output]').forEach((editor) => {
      editor.value = taskDrafts.get(editor.dataset.simulationOutput) ?? '{}';
    });
    content.querySelectorAll('[data-simulation-wait]').forEach((article) => {
      const { wait } = waits.find((entry) => entry.wait.jobId === article.dataset.simulationWait);
      article.querySelector('[data-simulation-wait-input]').value = JSON.stringify(wait.input ?? null, null, 2);
      const prepared = article.querySelector('[data-simulation-prepared-outputs]');
      if (prepared) prepared.value = JSON.stringify(fixtures.get(wait.nodeId).result.outputs ?? {}, null, 2);
      else {
        fillAnswerForm(article, drafts.get(wait.jobId) ?? blankServiceDraft());
        const errors = fieldErrors.get(wait.jobId);
        if (errors) showAnswerErrors(article, errors);
      }
    });
    fieldErrors.clear();
    const payloadEditor = messageField('payload');
    if (payloadEditor) payloadEditor.value = messageDraft.payload;
  };

  const showMessageErrors = (errors = {}) => {
    let first = null;
    content.querySelectorAll('[data-message-error]').forEach((alert) => {
      const error = errors[alert.dataset.messageError];
      if (error) {
        alert.setAttribute('message', errorMessage(error));
        first ??= alert;
      }
      alert.hidden = !error;
    });
    first?.scrollIntoView?.({ block: 'center' });
  };

  // The editor keeps Escape for itself (Escape, then Tab, leaves it); the window must not also see it.
  content.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && event.target.closest?.('tf-code-editor')) event.stopPropagation();
  });
  content.addEventListener('change', (event) => {
    const article = event.target.closest?.('[data-simulation-wait]');
    if (article) syncAnswerForm(article);
    // Choosing a wait fills the name and key it waits for and addresses exactly that wait.
    const pick = event.target.closest?.('[data-message-field="pick"]');
    if (pick) {
      const chosen = simulationMessageWaits(current.subscriptions).find((subscription) => subscription.subscriptionId === pick.value);
      if (chosen) {
        messageField('name').value = chosen.messageName;
        messageField('key').value = chosen.correlationKey;
      }
      messageField('exact').checked = !!chosen;
    }
  });

  content.addEventListener('click', (event) => {
    if (event.target.closest('[data-simulation-end]')) {
      ending = true;
      win.close(true);
      return;
    }
    const advance = event.target.closest('[data-simulation-advance]');
    if (advance) {
      if (simulationFinished(current.instance?.status)) return;
      run(async () => (await ApiBinary.one('processSimulationAdvanceRequest', { simulationId: current.simulationId })).view);
      return;
    }
    const address = event.target.closest('[data-message-address]');
    if (address) {
      const message = (current.messages || []).find((entry) => entry.messageId === address.dataset.messageAddress);
      if (!message || busy) return;
      messageField('pick').value = '';
      messageField('name').value = message.messageName;
      messageField('key').value = message.correlationKey;
      messageField('exact').checked = false;
      messageField('pick').scrollIntoView?.({ block: 'center' });
      return;
    }
    if (event.target.closest('[data-simulation-message-send]')) {
      if (busy || simulationFinished(current.instance?.status)) return;
      messageDraft = readMessageForm();
      const read = readMessageDraft(messageDraft, current.subscriptions);
      showMessageErrors(read.errors);
      if (read.errors) return;
      const simulationId = current.simulationId;
      run(async () => (await ApiBinary.one('processSimulationMessageSendRequest', { simulationId, ...read.request })).view);
      return;
    }
    const resolve = event.target.closest('[data-simulation-resolve]');
    if (resolve) {
      const article = resolve.closest('[data-simulation-wait]');
      const wait = (current.serviceWaits || []).find((entry) => entry.jobId === resolve.dataset.simulationResolve);
      if (!wait || busy || wait.mode === 'Fixture') return;
      const [{ step }] = simulationWaitSteps([wait], model);
      const answer = readServiceAnswer(step, readAnswerDraft(article));
      showAnswerErrors(article, answer.errors);
      if (answer.errors) return;
      const simulationId = current.simulationId;
      run(async () => (await ApiBinary.one('processSimulationServiceResolveRequest', {
        simulationId, nodeId: wait.nodeId, jobId: wait.jobId, result: answer.result, verdict: answer.verdict,
      })).view, (error) => {
        // The engine alone knows when an expression made a verdict necessary; it belongs at the verdict field.
        if (/SIMULATION_VERDICT_REQUIRED/.test(error?.message || '')) fieldErrors.set(wait.jobId, { verdict: 'simulation_verdict_required' });
      });
      return;
    }
    const taskButton = event.target.closest('[data-simulation-task]');
    if (!taskButton || busy) return;
    const simulationId = current.simulationId;
    const userTaskId = taskButton.dataset.simulationTask;
    // Only an open task has a decision left to take; a second press on a finished one does nothing.
    if (!(current.userTasks || []).some((task) => task.userTaskId === userTaskId && task.status === 'Open')) return;
    if (taskButton.dataset.simulationDecide) {
      const approved = taskButton.dataset.simulationDecide === 'true';
      run(async () => (await ApiBinary.one('processSimulationUserTaskCompleteRequest', { simulationId, userTaskId, outputs: {}, approved })).view);
      return;
    }
    if (taskButton.dataset.simulationManual === 'true') {
      run(async () => (await ApiBinary.one('processSimulationManualTaskAcknowledgeRequest', { simulationId, userTaskId })).view);
      return;
    }
    const article = taskButton.closest('[data-simulation-open-task]');
    const editor = article.querySelector('[data-simulation-output]');
    const alert = article.querySelector('[data-task-error]');
    const read = readOutputs(editor.value);
    alert.hidden = !read.error;
    if (read.error) {
      alert.setAttribute('message', errorMessage(read.error));
      return;
    }
    run(async () => (await ApiBinary.one('processSimulationUserTaskCompleteRequest', { simulationId, userTaskId, outputs: read.value })).view);
  });

  // A run that still waits for someone is not dropped by Escape or the backdrop; the button ends it on purpose.
  let asking = false;
  win.addEventListener('close-request', async (event) => {
    if (ending || asking || !simulationOpen(current)) return;
    event.preventDefault();
    asking = true;
    const end = await TfWindow.confirm({ title: text('simulation_end_title'), message: escapeHtml(text('simulation_end_hint')),
      confirmLabel: text('simulation_end'), cancelLabel: text('simulation_keep'), danger: true });
    asking = false;
    if (end && win.isConnected) {
      ending = true;
      win.close(true);
    }
  });
  if (options.flowId) {
    entry.snapshot = () => {
      captureDrafts();
      return { flowId: options.flowId, simulationId: current.simulationId, model, fixtures, drafts: [...drafts], taskDrafts: [...taskDrafts], messageDraft };
    };
    live.add(entry);
  }
  win.addEventListener('closed', () => {
    live.delete(entry);
    const simulationId = current?.simulationId;
    // A language switch remounts the screen: the run is kept for the new one, not released.
    if (entry.handedOver) {
      // The new screen already took the run over.
    } else if (simulationId && options.flowId && I18n.getLanguage() !== language && !ending) {
      captureDrafts();
      carried = { flowId: options.flowId, simulationId, model, fixtures, drafts: [...drafts], taskDrafts: [...taskDrafts] };
    } else if (simulationId) {
      void ApiBinary.one('processSimulationReleaseRequest', { simulationId }).catch((error) => {
        console.warn('[simulation] release request failed:', error);
      });
    }
    content.replaceChildren();
  }, { once: true });
  render();
  return win;
}

// Reopens the run a language switch closed, for the same process and as the server holds it now.
export async function resumeProcessSimulation(flowId) {
  for (const entry of live) {
    if (entry.language !== I18n.getLanguage() && !entry.handedOver) {
      entry.handedOver = true;
      carried = entry.snapshot();
    }
  }
  const kept = carried;
  carried = null;
  if (!kept) return null;
  try {
    if (kept.flowId !== flowId) throw new Error('another process');
    const { view } = await ApiBinary.one('processSimulationViewRequest', { simulationId: kept.simulationId });
    return simulationWindow(view, kept.model, { flowId, fixtures: kept.fixtures, drafts: kept.drafts, taskDrafts: kept.taskDrafts, messageDraft: kept.messageDraft });
  } catch (error) {
    if (kept.flowId === flowId) console.warn('[simulation] could not resume the run:', error);
    void ApiBinary.one('processSimulationReleaseRequest', { simulationId: kept.simulationId }).catch(() => {});
    return null;
  }
}

// One row per Service step the simulation can run: the operator answers when
// the step is reached, or a prepared result applies by itself. Running the
// step for real is listed but cannot be chosen, because the engine refuses it.
function fixturesSection(model) {
  const steps = simulationServiceSteps(model);
  if (!steps.length && !simulationUnsupportedSteps(model).length) return null;
  const names = processElementNames(model);
  const unsupported = simulationUnsupportedSteps(model);
  const labels = processDistinctLabels([...steps.map((step) => step.nodeId), ...unsupported.map((step) => step.nodeId)].map((id) => [id, names.get(id)]));
  const section = document.createElement('section');
  section.className = 'fb-simulation-fixtures';
  section.innerHTML = `<h3>${escapeHtml(text(steps.length ? 'simulation_fixtures' : 'simulation_not_simulated_heading'))}</h3>${steps.length ? `<p>${escapeHtml(text('simulation_fixtures_hint'))}</p>` : ''}
    ${unsupported.map((step) => `<article class="fb-simulation-fixture" data-fixture-unsupported="${escapeAttr(step.nodeId)}">
      <div><strong>${escapeHtml(labels.get(step.nodeId))}</strong> <tf-chip status="warn">${escapeHtml(text('simulation_not_simulated'))}</tf-chip></div>
      <p>${escapeHtml(text(`simulation_unsupported_${step.reason}`))}</p></article>`).join('')}
    ${steps.map((step) => `<article class="fb-simulation-fixture" data-fixture-step="${escapeAttr(step.nodeId)}">
      <strong>${escapeHtml(labels.get(step.nodeId))}</strong>
      <tf-select data-fixture-mode wrap-selected label="${escapeAttr(text('simulation_fixture_mode'))}" hint="${escapeAttr(text('simulation_mode_real_unavailable'))}" value="Result">
        <option value="Result">${escapeHtml(text('simulation_mode_result'))}</option>
        <option value="Fixture">${escapeHtml(text('simulation_mode_fixture'))}</option>
        <option value="RealExecution" disabled>${escapeHtml(text('simulation_mode_real'))}</option>
      </tf-select>
      <div data-fixture-form hidden>${answerFormHtml(step, blankServiceDraft())}</div>
    </article>`).join('')}`;
  section.querySelectorAll('[data-fixture-step]').forEach((article) => fillAnswerForm(article, blankServiceDraft()));
  section.addEventListener('change', (event) => {
    const mode = event.target.closest('[data-fixture-mode]');
    if (mode) mode.closest('[data-fixture-step]').querySelector('[data-fixture-form]').hidden = mode.value !== 'Fixture';
    const article = event.target.closest('[data-fixture-step]');
    if (article) syncAnswerForm(article);
  });
  // The fixtures to send, or the first problems marked inside the forms.
  section.readFixtures = () => {
    const articles = [...section.querySelectorAll('[data-fixture-step]')];
    const choices = new Map(articles.map((article) => [article.dataset.fixtureStep,
      { mode: article.querySelector('[data-fixture-mode]').value, draft: readAnswerDraft(article) }]));
    const read = readServiceFixtures(steps, choices);
    articles.forEach((article) => showAnswerErrors(article, read.errors.get(article.dataset.fixtureStep) ?? {}));
    return read;
  };
  return section;
}

export function openProcessSimulation(definition, version, startCatalog, selectedStart, anchor = null) {
  const manualStarts = startCatalog.filter((entry) => entry.trigger?.Start?.canStart);
  const selection = document.createElement('section');
  selection.className = 'fb-process-simulation-start';
  const startLabels = processStartEntryLabels(manualStarts, definition.model);
  selection.innerHTML = `<tf-select data-simulation-start wrap-selected label="${escapeAttr(text('start_entry'))}"><option value="">${escapeHtml(text('start_entry'))}</option>${manualStarts.map((entry, index) => `<option value="${index}">${escapeHtml(startLabels.get(index))}</option>`).join('')}</tf-select>`;
  const variables = jsonSection(text('simulation_variables'), definition.model.variables || {});
  const fixtures = fixturesSection(definition.model);
  const selectedIndex = manualStarts.findIndex((entry) => entry.processId === selectedStart?.processId
    && entry.startNodeId === selectedStart?.startNodeId);
  selection.querySelector('[data-simulation-start]').value = selectedIndex < 0 ? '' : String(selectedIndex);
  // The simulation clock cannot run a repetition, so the window says so before
  // the author starts it instead of after the server refuses.
  const repeated = [...new Set(processRepeatedElements(definition.model))];
  return openFormWindow({
    title: text('simulation'), icon: 'play', subject: definition.name,
    note: repeated.length ? { tone: 'warning', text: text('simulation_repeat_unsupported', { names: repeated.join(', ') }) } : { text: text('simulation_hint') },
    sections: [selection, variables, ...(fixtures ? [fixtures] : [])],
    width: 720,
    submitLabel: text('simulation_start'), busyLabel: text('simulation_starting'), anchor,
    errorMessage: (error) => simulationErrorText(error?.message, definition.model),
    validate: () => {
      const selected = selection.querySelector('[data-simulation-start]').value;
      if (selected !== '') {
        try {
          variables.readValue();
        } catch (_) {
          return false;
        }
        return !fixtures || fixtures.readFixtures().errors.size === 0;
      }
      selection.querySelector('[data-simulation-start]').setAttribute('error', text('start_entry_changed'));
      return false;
    },
    canSubmit: () => selection.querySelector('[data-simulation-start]').value !== '' && !definition.archived && repeated.length === 0,
    collect: () => {
      const entry = manualStarts[Number(selection.querySelector('[data-simulation-start]').value)];
      if (!entry) throw new Error(text('start_entry_changed'));
      return { entry, variables: variables.readValue(), serviceFixtures: fixtures ? fixtures.readFixtures().fixtures : [] };
    },
    onSubmit: async ({ entry, variables: initialVariables, serviceFixtures }) => {
      const startMs = Date.now();
      const response = await ApiBinary.one('processSimulationStartRequest', {
        definitionId: definition.definitionId,
        version: version.version,
        selectedProcessId: entry.processId,
        startNodeId: entry.startNodeId,
        variables: initialVariables,
        startMs,
        horizonMs: startMs + 60 * 60 * 1000,
        tickDurationMs: 1000,
        serviceFixtures,
      });
      simulationWindow(response.view, definition.model, {
        flowId: definition.definitionId,
        fixtures: new Map(serviceFixtures.map((fixture) => [fixture.nodeId, fixture])),
      });
      return {};
    },
  });
}
