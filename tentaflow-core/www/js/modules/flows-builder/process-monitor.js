// ============ File: flows-builder/process-monitor.js — authorized process runs, human work and durable history ============

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { checkProcessValue, processBody, processCommand, processEditorLabels, processJson, processStatusLabel } from './bpmn.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-select.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-toggle.js';

const text = (key, values) => I18n.t(`bpmn.${key}`, values);
const date = (ms, timeZone) => new Date(ms).toLocaleString(I18n.getLanguage(), timeZone ? { timeZone } : undefined);
const tone = (status) => status === 'Completed' ? 'ok' : ['Incident', 'Error'].includes(status) ? 'err' : status === 'Cancelled' ? 'neutral' : 'info';

function workingDate(ms, summary, zone) {
  if (summary?.dueOffsetSeconds == null) return new Date(ms).toISOString();
  const offset = summary.dueOffsetSeconds;
  const magnitude = Math.abs(offset);
  const hours = String(Math.floor(magnitude / 3600)).padStart(2, '0');
  const minutes = String(Math.floor(magnitude % 3600 / 60)).padStart(2, '0');
  const seconds = magnitude % 60;
  const signedOffset = `${offset < 0 ? '-' : '+'}${hours}:${minutes}${seconds ? `:${String(seconds).padStart(2, '0')}` : ''}`;
  const local = new Date(Number(ms) + offset * 1000).toLocaleString(I18n.getLanguage(), { timeZone: 'UTC' });
  return text('calendar_due_exact', { local, offset: signedOffset, utc: new Date(ms).toISOString(), zone });
}

function readWindow(title, icon, width = 820) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', title);
  win.setAttribute('icon', icon);
  win.setAttribute('modal', '');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', String(width));
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.classList.add('fb-process-window');
  win.innerHTML = `<div slot="body" class="fb-process-content"><tf-alert data-error tone="danger" hidden></tf-alert><div data-content></div></div>`;
  document.body.appendChild(win);
  return win;
}

function showError(win, error) {
  if (!win.isConnected) return;
  const el = win.querySelector('[data-error]');
  el.hidden = false;
  el.setAttribute('message', text('request_error', { error: error.message }));
}

function jsonSection(label, value, editable = true) {
  const section = document.createElement('div');
  section.className = 'fb-process-json';
  section.innerHTML = `<h3>${escapeHtml(label)}</h3><tf-code-editor language="json" aria-label="${escapeAttr(label)}" ${editable ? '' : 'readonly'}></tf-code-editor><tf-alert data-json-error tone="danger" hidden></tf-alert>`;
  const editor = section.querySelector('tf-code-editor');
  editor.labels = processEditorLabels();
  editor.value = JSON.stringify(value, null, 2);
  return section;
}

function activityInputsSection(task) {
  if (!Array.isArray(task.activityInputs) || task.activityInputs.length === 0) return null;
  const section = document.createElement('section');
  section.className = 'fb-process-activity-inputs';
  section.innerHTML = `<h3>${escapeHtml(text('activity_input_values'))}</h3>
    <p class="fb-process-activity-inputs-hint">${escapeHtml(text('activity_input_values_hint'))}</p>`;
  for (const input of task.activityInputs) {
    const row = document.createElement('div');
    row.className = 'fb-process-activity-input';
    const label = input.name || input.declarationId || text('activity_input_unnamed');
    const title = document.createElement('h4');
    title.textContent = `${label} · ${input.declarationId || ''}`;
    row.append(title);
    const missing = input.value === 'Missing';
    if (missing) {
      const chip = document.createElement('tf-chip');
      chip.setAttribute('status', 'neutral');
      chip.textContent = text('activity_input_missing');
      row.append(chip);
    } else {
      const editor = document.createElement('tf-code-editor');
      editor.setAttribute('language', 'json');
      editor.setAttribute('readonly', '');
      editor.setAttribute('aria-label', label);
      editor.labels = processEditorLabels();
      const present = input.value !== null && typeof input.value === 'object'
        && Object.hasOwn(input.value, 'Present');
      const value = present ? input.value.Present : null;
      editor.value = JSON.stringify(value, null, 2);
      row.append(editor);
    }
    section.append(row);
  }
  return section;
}

export function openProcessCallPreview(version, processId) {
  const win = readWindow(text('call_preview'), 'layers', 940);
  const host = win.querySelector('[data-content]');
  host.innerHTML = `<h2>${escapeHtml(text('version_number', { version: version.version }))}</h2>
    <p>${escapeHtml(text('call_preview_readonly'))}</p>`;
  host.append(jsonSection(text('call_published_model'), processBody(version.model, [], processId), false));
  return win;
}

function validateJson(section, object = false) {
  const error = section.querySelector('[data-json-error]');
  try {
    const value = section.querySelector('tf-code-editor').value;
    section.jsonValue = object ? processJson(value) : JSON.parse(value);
    if (!object) checkProcessValue(section.jsonValue);
    error.hidden = true;
    return true;
  } catch (failure) {
    error.setAttribute('message', failure instanceof SyntaxError ? text(object ? 'object_required' : 'json_invalid') : failure.message);
    error.hidden = false;
    return false;
  }
}

export function processEventText(event) {
  const node = event.nodeName || text('element_unavailable');
  if (event.kind === 'terminate_end_reached') return text('event_terminate_end_reached', { node });
  if (event.kind === 'instance_completed' && event.data?.reason === 'terminate_end') return text('event_instance_terminated');
  if (event.kind === 'cancelled' && event.data?.reason === 'terminate_end') return text('event_instance_cancelled_terminate');
  if (event.kind === 'scope_completed' && event.data?.reason === 'terminate_end') return text('event_scope_terminated', {
    element: event.data.subprocess_node_id || '',
  });
  if (event.kind === 'call_request_cancelled') return text('event_call_request_cancelled', {
    node, reason: processLifecycleReasonText(event.data.reason || ''),
  });
  if (event.kind === 'timer_armed') return text('event_timer_armed', { node,
    due: event.data.working_time ? workingDate(event.data.due_at_ms, {
      dueOffsetSeconds: event.data.working_time.due_offset_seconds,
    }, event.data.timezone) : date(event.data.due_at_ms, event.data.timezone), timezone: event.data.timezone });
  if (event.kind === 'timer_fired') {
    const due = event.data.working_time ? workingDate(event.data.planned_due_at_ms, {
      dueOffsetSeconds: event.data.working_time.due_offset_seconds,
    }, event.data.timezone) : date(event.data.planned_due_at_ms);
    if (event.data.kind === 'Boundary') return text('event_boundary_fired', {
      node, due, actual: event.data.working_time ? new Date(event.data.fired_at_ms).toISOString() : date(event.data.fired_at_ms),
      mode: text(event.data.cancel_activity ? 'boundary_interrupting' : 'boundary_noninterrupting'),
    });
    return text('event_timer_fired', { node, due, actual: event.data.working_time ? new Date(event.data.fired_at_ms).toISOString() : date(event.data.fired_at_ms), count: event.data.skipped_count });
  }
  if (event.kind === 'timer_cancelled') {
    if (event.data.reason === 'instance_cancelled') return text('event_timer_cancelled', { node });
    return text('event_timer_cancelled_reason', { node, reason: processLifecycleReasonText(event.data.reason) });
  }
  if (event.kind === 'timer_blocked') return text('event_timer_blocked', { node, message: event.data.reason, retry: date(event.data.next_check_at_ms) });
  if (event.kind === 'timer_error') return text('event_timer_error', { node, message: event.data.reason });
  if (event.kind === 'service_result') return text('event_service_result', { node, summary: event.data.summary });
  if (event.kind === 'script_completed') return text('event_script_completed', { node });
  if (event.kind === 'manual_task_opened') return text('event_manual_task_opened', { node });
  if (event.kind === 'manual_task_acknowledged') return text('event_manual_task_acknowledged', { node });
  if (event.kind === 'send_task_admitted') return text('event_send_task_admitted', {
    node, message: event.data.message_name, key: event.data.correlation_key,
  });
  if (event.kind === 'send_task_pending') return text('event_send_task_pending', { node });
  if (event.kind === 'send_admission_failed') return text('event_send_admission_failed', {
    node, reason: processIncidentText({ code: event.data.code, message: event.data.reason }),
  });
  if (event.kind === 'receive_task_opened') return text('event_receive_task_opened', {
    node, message: event.data.message_name, key: event.data.correlation_key,
  });
  if (event.kind === 'receive_task_completed') return text('event_receive_task_completed', {
    node, message: event.data.message_id,
  });
  if (event.kind === 'signal_admitted') return text('event_signal_admitted', { node });
  if (event.kind === 'signal_catch_opened') return text('event_signal_catch_opened', { node });
  if (event.kind === 'signal_received') return text('event_signal_received', { node });
  if (event.kind === 'link_thrown') return text('event_link_thrown', { node,
    target: event.data.catch_node_id });
  if (event.kind === 'link_caught') return text('event_link_caught', { node,
    source: event.data.source_token_id });
  if (event.kind === 'inclusive_split') return text('event_inclusive_split', { node,
    count: event.data.selected_branch_edge_ids.length,
    default: event.data.default_selected ? text('inclusive_default_selected') : '' });
  if (event.kind === 'inclusive_joined') return text('event_inclusive_joined', { node,
    count: event.data.selected_branch_edge_ids.length });
  if (event.kind === 'repetition_group_blocked') return text(
    event.data.phase === 'capacity' ? 'event_repetition_group_blocked_capacity' : 'event_repetition_group_blocked_aggregate',
    { node, code: processIncidentText(event.data), reason: processLifecycleReasonText(event.data.reason || '') },
  );
  if (event.kind === 'repetition_entry_failed') return text('event_repetition_entry_failed', {
    node, code: processIncidentText(event.data),
  });
  if (event.kind.startsWith('message_')) return text(`event_${event.kind}`, {
    node, message: event.data.message_name || event.data.message_id || '',
    reason: processLifecycleReasonText(event.data.reason || ''), key: event.data.correlation_key || '',
  });
  if (event.kind.startsWith('event_race_')) return text(`event_${event.kind}`, {
    node, winner: event.data.winner_node_id || '', reason: processLifecycleReasonText(event.data.reason || ''),
  });
  if (event.kind === 'subscription_cancelled') return text('event_subscription_cancelled', {
    node, reason: processLifecycleReasonText(event.data.reason || ''),
  });
  if (event.kind === 'business_error_caught') return text('event_business_error_caught', {
    node, code: event.data.error_code || text('error_catch_all_hint'),
  });
  if (event.kind === 'escalation_boundary_armed') return text('event_escalation_boundary_armed', {
    node, code: event.data.escalation_code || text('escalation_any_code'),
  });
  if (event.kind === 'escalation_caught') return text('event_escalation_caught', {
    node, code: event.data.code || text('escalation_no_code'),
    matched: event.data.matched_escalation_code || text('escalation_any_code'),
  });
  if (['scope_entered', 'scope_completed', 'scope_cancelled'].includes(event.kind)) return text(`event_${event.kind}`, {
    element: event.data.subprocess_node_id || '',
    reason: processLifecycleReasonText(event.data.reason || ''),
  });
  if (event.kind === 'scope_entry_failed') return text('event_scope_entry_failed', {
    node, reason: processLifecycleReasonText(event.data.reason || ''),
  });
  if (event.kind === 'scope_error_propagated') return text('event_scope_error_propagated', {
    node, code: event.data.code || '',
  });
  if (['call_requested', 'call_entered', 'call_returned', 'call_cancelled'].includes(event.kind)) {
    return text(`event_${event.kind}`, { node, reason: processLifecycleReasonText(event.data.reason || ''),
      version: event.data.called_version || '', child: event.data.child_instance_id || '' });
  }
  if (event.kind === 'call_error_propagated') {
    const source = event.data.source_kind === 'error_end' ? text('node_error_end')
      : event.data.source_kind === 'contract' ? text('node_service_task') : event.data.source_kind;
    return text('event_call_error_propagated', { node, code: event.data.error_code, source });
  }
  if (event.kind === 'error_end_reached') return text('event_error_end_reached', {
    node, code: event.data.error_code, source: event.data.source_node_id,
  });
  if (event.kind === 'instance_error') return text('event_instance_error', {
    node, code: event.data.error_code,
  });
  if (['incident', 'job_denied', 'job_failed'].includes(event.kind)) return text(`event_${event.kind}`, { node, message: processIncidentText(event.data) });
  return text(`event_${event.kind}`, { node });
}

export function processTimerText(timer) {
  const due = timer.dueAtMs == null ? text('timer_no_due') : timer.workingTime
    ? workingDate(timer.dueAtMs, timer.workingTime, timer.timezone)
    : new Date(timer.dueAtMs).toLocaleString(I18n.getLanguage(), { timeZone: timer.timezone, timeZoneName: 'short' });
  return text('timer_brief', { status: text(`timer_status_${timer.status.toLowerCase()}`), due, timezone: timer.timezone });
}

export function processLifecycleReasonText(reason) {
  switch (reason) {
    case 'instance_cancelled': return text('event_cancelled');
    case 'activity_completed': return text('timer_reason_activity_completed');
    case 'sibling_interrupted': return text('timer_reason_sibling_interrupted');
    case 'event_race_lost': return text('timer_reason_event_race_lost');
    case 'scope_cancelled': return text('reason_scope_cancelled');
    case 'call_interrupted': return text('reason_call_interrupted');
    case 'error_end': return text('reason_error_end');
    case 'terminate_end': return text('reason_terminate_end');
    case 'child_cancelled': return text('reason_child_cancelled');
    case 'scope_limit': return text('reason_scope_limit');
    case 'definition_archived': return text('timer_reason_definition_archived');
    case 'missed_during_archive': return text('timer_reason_missed_during_archive');
    case 'finite_schedule_exhausted_during_archive': return text('timer_reason_finite_schedule_exhausted_during_archive');
    case 'superseded_by_publication': return text('timer_reason_superseded_by_publication');
    case 'sender_cancelled': return text('reason_sender_cancelled');
    case 'ttl_expired': return text('reason_ttl_expired');
    case 'activation_closed': return text('reason_activation_closed');
    case 'target_instance_closed': return text('reason_target_instance_closed');
    case 'active_occurrences': return text('repeat_reason_active_occurrences');
    case 'active_service_jobs': return text('repeat_reason_active_service_jobs');
    case 'group_lifetime': return text('repeat_reason_group_lifetime');
    case 'occurrence_lifetime': return text('repeat_reason_occurrence_lifetime');
    case 'repetition_bytes': return text('repeat_reason_repetition_bytes');
    default: return reason;
  }
}

function renderTimers(host, timers) {
  host.replaceChildren();
  for (const timer of timers) {
    const row = document.createElement('div');
    row.className = 'fb-process-work fb-process-timer';
    row.dataset.timerId = timer.timerId;
    row.innerHTML = `<div><strong>${escapeHtml(timer.nodeName || text(timer.kind === 'Start' ? 'node_timer_start' : timer.kind === 'Boundary' ? 'node_boundary_timer' : 'node_timer_catch'))}</strong>
      <p>${escapeHtml(processTimerText(timer))}</p><dl><dt>${escapeHtml(text('timer_occurrence'))}</dt><dd>${escapeHtml(timer.totalFirings == null ? String(timer.occurrence) : text('timer_slot', { occurrence: timer.occurrence, total: timer.totalFirings }))}</dd>
      ${timer.kind === 'Boundary' ? `<dt>${escapeHtml(text('boundary_attached_element_id'))}</dt><dd>${escapeHtml(timer.attachedToId)}</dd>` : ''}
      ${timer.workingTime ? `<dt>${escapeHtml(text('calendar_name'))}</dt><dd>${escapeHtml(timer.workingTime.calendarName)}</dd>
      <dt>${escapeHtml(text('calendar_holiday_policy'))}</dt><dd>${escapeHtml(text(timer.workingTime.holidayPolicy === 'None' ? 'calendar_policy_none' : 'calendar_policy_poland'))}</dd>
      <dt>${escapeHtml(text('calendar_release'))}</dt><dd>${escapeHtml(text('calendar_pin_info', {
        release: timer.workingTime.legalReleaseId, asOf: timer.workingTime.legalAsOfDate,
        timezone: timer.workingTime.tzdbReleaseId, digest: timer.workingTime.pinSha256,
      }))}</dd>` : ''}
      ${timer.lastReason ? `<dt>${escapeHtml(text('timer_reason'))}</dt><dd>${escapeHtml(processLifecycleReasonText(timer.lastReason))}</dd>` : ''}</dl></div>`;
    host.append(row);
  }
}

export async function openProcessSchedule(definitionId) {
  const win = readWindow(text('timer_schedule'), 'clock');
  const host = win.querySelector('[data-content]');
  host.innerHTML = `<div data-schedule-summary></div><p>${escapeHtml(text('timer_schedule_hint'))}</p><section data-timers></section>
    <tf-button variant="secondary" icon="clock" data-schedule-instances>${escapeHtml(text('instances'))}</tf-button>`;
  let refreshing = false;
  let generation = 0;
  async function refresh() {
    if (refreshing || !win.isConnected) return;
    refreshing = true;
    const current = ++generation;
    try {
      const response = await ApiBinary.one('processDefinitionGetRequest', { definitionId });
      if (!win.isConnected || current !== generation) return;
      host.querySelector('[data-schedule-summary]').textContent = `${response.definition.name} · ${response.definition.publishedVersion ? text('version_number', { version: response.definition.publishedVersion }) : text('draft')}`;
      const timers = host.querySelector('[data-timers]');
      const persisted = response.startCatalog
        .filter((entry) => entry.trigger?.TimerStart?.persistedTimer != null)
        .map((entry) => entry.trigger.TimerStart.persistedTimer);
      if (persisted.length) renderTimers(timers, persisted);
      else timers.textContent = text('timer_schedule_empty');
      host.querySelector('[data-schedule-instances]').removeAttribute('disabled');
      win.querySelector('[data-error]').hidden = true;
    } catch (error) {
      if (!win.isConnected || current !== generation) return;
      host.querySelector('[data-timers]').replaceChildren();
      host.querySelector('[data-schedule-instances]').setAttribute('disabled', '');
      showError(win, error);
    } finally { refreshing = false; }
  }
  host.querySelector('[data-schedule-instances]').addEventListener('click', () => openProcessInstances(definitionId));
  await refresh();
  if (!win.isConnected) return win;
  const timer = setInterval(refresh, 3000);
  win.addEventListener('closed', () => { clearInterval(timer); generation += 1; }, { once: true });
  return win;
}

function processIncidentText(incident) {
  if (incident.code === 'TIMER_ERROR') return text('incident_timer_error', { reason: incident.message });
  if (incident.code === 'INCLUSIVE_GATEWAY_ERROR') return `${text('incident_inclusive_gateway_error')} ${text('incident_inclusive_guidance')}`;
  if (incident.code === 'ESCALATION_HANDLER_FAILED') return `${text('incident_escalation_handler_failed')} ${text('incident_escalation_guidance')}`;
  if (incident.code === 'SCRIPT_EVALUATION_FAILED' || incident.code === 'SCRIPT_MAPPING_FAILED') {
    return `${text(`incident_${incident.code.toLowerCase()}`)} ${text('incident_script_guidance')}`;
  }
  const codes = ['EXPRESSION_ERROR', 'AMBIGUOUS_GATEWAY', 'NO_MATCHING_FLOW', 'HUMAN_REJECTED', 'SERVICE_ERROR', 'VERIFICATION_FAILED', 'WORKER_ERROR', 'FLOW_ERROR', 'INVALID_SERVICE_JOB', 'SOURCE_ACCESS_REVOKED', 'INTERRUPTED', 'LEASE_LOST', 'SERVICE_TIMEOUT', 'OUTPUT_LIMIT', 'TRANSITION_ERROR', 'RESULT_REJECTED', 'REVISION_CONFLICT', 'SCOPE_LIMIT', 'REPETITION_INPUT_ERROR', 'REPETITION_LIMIT', 'REPETITION_AGGREGATE_LIMIT', 'REPETITION_MAPPING_FAILED', 'SCRIPT_INFRASTRUCTURE_FAILED', 'MESSAGE_EXPRESSION_ERROR', 'SEND_ADMISSION_AUTHORITY_DENIED', 'SEND_ADMISSION_TARGET_UNAVAILABLE', 'IMMEDIATE_TRANSITION_LIMIT'];
  return codes.includes(incident.code) ? text(`incident_${incident.code.toLowerCase()}`) : (incident.message || text('incident_generic'));
}

export async function openProcessInstances(definitionId = null) {
  const win = readWindow(text('instances'), 'clock');
  let offset = 0;
  let generation = 0;
  const host = win.querySelector('[data-content]');
  host.innerHTML = `<tf-table data-instances empty-message="${escapeAttr(text('instances_empty'))}" page-size="25" page="1">
    <tf-column key="name" label="${escapeAttr(text('process_name'))}"></tf-column>
    <tf-column key="versionLabel" label="${escapeAttr(text('version'))}"></tf-column>
    <tf-column key="statusLabel" label="${escapeAttr(text('status'))}"></tf-column>
    <tf-column key="updated" label="${escapeAttr(text('updated'))}"></tf-column>
  </tf-table>`;
  const table = host.querySelector('tf-table');
  table.rowKey = 'instanceId';
  table.rowActions = (_row, _index, current) => {
    const button = document.createElement('tf-button');
    button.setAttribute('variant', 'secondary');
    button.setAttribute('size', 'sm');
    button.textContent = text('open_instance');
    button.addEventListener('click', () => openProcessInstance(current().instanceId));
    return button;
  };
  table.rowActionsKey = (row) => row.instanceId;
  async function load() {
    const current = ++generation;
    try {
      const response = await ApiBinary.one('processInstanceListRequest', { definitionId, offset, limit: 25 });
      if (!win.isConnected || current !== generation) return;
      table.setAttribute('total', String(response.total));
      table.setAttribute('page', String(offset / 25 + 1));
      table.rows = response.instances.map((instance) => ({ ...instance, name: instance.definitionName,
        versionLabel: text('version_number', { version: instance.version }), statusLabel: processStatusLabel(instance.status), updated: date(instance.updatedAtMs) }));
    } catch (error) { if (current === generation) showError(win, error); }
  }
  table.addEventListener('page-change', (event) => { offset = (event.detail.page - 1) * 25; load(); });
  await load();
  return win;
}

export async function openProcessInstance(instanceId, initial = null) {
  const win = readWindow(text('instance'), 'play', 940);
  const host = win.querySelector('[data-content]');
  host.innerHTML = `<div data-summary></div><section data-work></section><section data-incidents></section><section data-timer-section><h3>${escapeHtml(text('timers'))}</h3><div data-timers></div></section>
    <section data-calls><h3>${escapeHtml(text('calls'))}</h3><div data-call-rows></div></section>
    <section data-scopes><h3>${escapeHtml(text('scopes'))}</h3><div data-scope-rows></div><div data-scope-detail></div></section>
    <section data-repetition-groups><h3>${escapeHtml(text('repeat_groups'))}</h3><div data-repetition-group-rows></div></section>
    <section data-repetition-occurrences><h3>${escapeHtml(text('repeat_occurrences'))}</h3><div data-repetition-occurrence-rows></div><div data-repetition-detail></div></section>
    <section data-subscriptions><h3>${escapeHtml(text('event_subscriptions'))}</h3><div data-subscription-rows></div></section>
    <section data-event-races><h3>${escapeHtml(text('event_races'))}</h3><div data-race-rows></div></section>
    <section data-outgoing><h3>${escapeHtml(text('outgoing_messages'))}</h3><div data-outgoing-rows></div></section>
    <div data-variables></div><section class="fb-process-history"><h3>${escapeHtml(text('history'))}</h3><ol data-events></ol>
      <tf-button variant="secondary" data-more>${escapeHtml(text('more_history'))}</tf-button></section>`;
  let instance = initial;
  let renderedSnapshot = null;
  let available = true;
  let readGeneration = 0;
  let refreshing = false;
  let historyBusy = false;
  let afterSeq = 0;
  let historyEnded = false;
  let mutation = false;
  const cancelCommand = processCommand();
  const retryCommand = processCommand();
  const events = host.querySelector('[data-events]');
  const workWindows = new Set();
  const collectionNames = ['userTasks', 'incidents', 'timers', 'subscriptions', 'eventRaces', 'outgoingMessages', 'scopes', 'calls', 'repetitionGroups', 'repetitionOccurrences'];
  const pageSpecs = Object.fromEntries(collectionNames.map((name) => [name, { offset: 0, limit: 20 }]));
  let selectedUserTaskId = null;
  let selectedIncidentId = null;
  let selectedScopeId = null;
  let selectedRepetitionGroupId = null;
  let selectedRepetitionOccurrenceId = null;
  let selectedRepetitionValue = null;
  let scopeReadGeneration = 0;
  const requestPages = () => ({ ...pageSpecs, selectedUserTaskId, selectedIncidentId,
    selectedRepetitionGroupId, selectedRepetitionOccurrenceId, selectedRepetitionValue });
  const requestInstance = () => ApiBinary.one('processInstanceGetRequest', { instanceId, pages: requestPages() });

  function syncWorkWindows() {
    for (const workWindow of workWindows) workWindow.dispatchEvent(new CustomEvent('change', { bubbles: true }));
  }

  async function acceptSnapshot(snapshot) {
    if (!win.isConnected) return;
    if (instance && snapshot.revision < instance.revision) {
      const response = await requestInstance();
      if (!win.isConnected || response.instance.revision < instance.revision) return;
      snapshot = response.instance;
    }
    instance = snapshot;
    available = true;
    if (renderedSnapshot !== JSON.stringify(instance)) render();
    syncWorkWindows();
  }

  async function loadHistory() {
    if (historyBusy || !win.isConnected) return;
    historyBusy = true;
    try {
      const response = await ApiBinary.one('processHistoryRequest', { instanceId, afterSeq, limit: 50 });
      if (!win.isConnected) return;
      for (const event of response.events) {
        const item = document.createElement('li');
        item.dataset.processSeq = String(event.seq);
        const description = document.createElement('div');
        description.textContent = processEventText(event);
        const at = document.createElement('time');
        at.dateTime = new Date(event.atMs).toISOString();
        at.textContent = date(event.atMs);
        item.append(description, at);
        if (event.kind === 'service_result') item.append(jsonSection(text('actual_outputs'), event.data, false));
        if (event.kind === 'script_completed') item.append(jsonSection(text('script_result'), event.data.outputs, false));
        if (event.kind.startsWith('repetition_')) item.append(jsonSection(text('repeat_facts'), event.data, false));
        if (event.kind === 'terminate_end_reached') item.append(jsonSection(text('terminate_facts'), event.data, false));
        if (event.kind === 'message_delivered' && Object.hasOwn(event.data, 'payload')) item.append(jsonSection(text('message_payload'), event.data.payload, false));
        events.appendChild(item);
      }
      afterSeq = response.nextSeq;
      historyEnded = !response.hasMore;
      host.querySelector('[data-more]').hidden = historyEnded;
    } catch (error) { showError(win, error); }
    finally { historyBusy = false; }
  }

  async function applyMutation(request, payload, command) {
    if (mutation || !available || !win.isConnected) return;
    mutation = true;
    readGeneration += 1;
    host.setAttribute('inert', '');
    try {
      const response = await ApiBinary.one(request, command(payload));
      if (!win.isConnected) return;
      await acceptSnapshot(response.instance);
      if (historyEnded) await loadHistory();
    } catch (error) { showError(win, error); }
    finally { mutation = false; host.removeAttribute('inert'); await refresh(); }
  }

  async function complete(summary) {
    if (!available || !summary.canComplete || summary.status !== 'Open') return;
    let task;
    try {
      const response = await ApiBinary.one('processUserTaskGetRequest', { instanceId, userTaskId: summary.userTaskId });
      if (!win.isConnected) return;
      task = response.task;
    } catch (error) { showError(win, error); return; }
    if (!task.canComplete || task.status !== 'Open') { await refresh(); return; }
    if (task.kind === 'Manual' && (typeof task.instructions !== 'string' || task.instructions.length === 0)) {
      showError(win, new Error(text('manual_instructions')));
      return;
    }
    selectedUserTaskId = task.userTaskId;
    let revision = instance.revision;
    if (task.kind === 'Manual') {
      const instructions = document.createElement('section');
      instructions.className = 'fb-process-work';
      instructions.innerHTML = `<tf-textarea readonly autogrow rows="3" label="${escapeAttr(text('manual_instructions'))}" value="${escapeAttr(task.instructions)}"></tf-textarea>`;
      const command = processCommand();
      const inputSection = activityInputsSection(task);
      const workWindow = openFormWindow({ title: text('acknowledge_manual'), icon: 'check', subject: task.name,
        note: { text: text('manual_acknowledgment_notice') }, sections: [instructions, inputSection].filter(Boolean),
        submitLabel: text('acknowledge_manual'),
        canSubmit: () => available && win.isConnected && [instance.selectedUserTask, ...instance.userTasks]
          .some((current) => current?.userTaskId === task.userTaskId && current.canComplete && current.status === 'Open'),
        collect: () => ({ instanceId, userTaskId: task.userTaskId, expectedRevision: revision }),
        onSubmit: async (payload) => {
          try {
            const response = await ApiBinary.one('processManualTaskAcknowledgeRequest', command(payload));
            if (win.isConnected) { await acceptSnapshot(response.instance); await refresh(); if (historyEnded) await loadHistory(); }
            return { message: text('manual_acknowledged') };
          } catch (error) {
            if (error.code === 'BadRequest') { await refresh(); revision = instance.revision; }
            throw error;
          }
        },
      });
      workWindows.add(workWindow);
      workWindow.addEventListener('closed', () => { workWindows.delete(workWindow); if (selectedUserTaskId === task.userTaskId) selectedUserTaskId = null; }, { once: true });
      return;
    }
    const outputs = jsonSection(text(task.kind === 'Verification' ? 'actual_outputs' : 'outputs'), task.outputs, task.kind !== 'Verification');
    const approval = document.createElement('div');
    if (task.kind === 'Verification') approval.innerHTML = `<tf-select label="${escapeAttr(text('review_result'))}" data-approved>
      <option value="">${escapeHtml(text('choose_review'))}</option><option value="true">${escapeHtml(text('approve_result'))}</option><option value="false">${escapeHtml(text('reject_result'))}</option></tf-select>`;
    const command = processCommand();
    const inputSection = activityInputsSection(task);
    const workWindow = openFormWindow({ title: text(task.kind === 'Verification' ? 'verification_human' : 'complete_work'), icon: 'check', subject: task.name,
      note: { text: text(task.kind === 'Verification' ? 'review_hint' : 'work_hint') },
      sections: [inputSection, outputs, approval].filter(Boolean), submitLabel: text('complete_work'),
      validate: () => task.kind === 'Verification' || validateJson(outputs),
      canSubmit: () => available && win.isConnected && [instance.selectedUserTask, ...instance.userTasks].some((current) => current?.userTaskId === task.userTaskId && current.canComplete && current.status === 'Open')
        && (task.kind !== 'Verification' || approval.querySelector('tf-select').value !== ''),
      collect: () => ({ instanceId, userTaskId: task.userTaskId, expectedRevision: revision,
        outputs: task.kind === 'Verification' ? {} : outputs.jsonValue, approved: task.kind === 'Verification' ? approval.querySelector('tf-select').value === 'true' : null }),
      onSubmit: async (payload) => {
        try {
          const response = await ApiBinary.one('processUserTaskCompleteRequest', command(payload));
          if (win.isConnected) { await acceptSnapshot(response.instance); await refresh(); if (historyEnded) await loadHistory(); }
          return { message: text('work_completed') };
        } catch (error) {
          if (error.code === 'BadRequest') { await refresh(); revision = instance.revision; }
          throw error;
        }
      },
    });
    workWindows.add(workWindow);
    workWindow.addEventListener('closed', () => { workWindows.delete(workWindow); if (selectedUserTaskId === task.userTaskId) selectedUserTaskId = null; }, { once: true });
  }

  function renderPageControls(container, name) {
    container.querySelector('[data-page-controls]')?.remove();
    const info = instance.pages[name];
    const controls = document.createElement('div');
    controls.className = 'fb-process-page';
    controls.dataset.pageControls = name;
    controls.innerHTML = `<span>${escapeHtml(text('page_position', { first: info.total ? info.offset + 1 : 0,
      last: Math.min(info.offset + pageSpecs[name].limit, info.total), total: info.total }))}</span>
      <tf-button variant="secondary" data-page-prev ${info.offset === 0 ? 'disabled' : ''}>${escapeHtml(text('previous_page'))}</tf-button>
      <tf-button variant="secondary" data-page-next ${info.hasMore ? '' : 'disabled'}>${escapeHtml(text('next_page'))}</tf-button>`;
    controls.querySelector('[data-page-prev]').addEventListener('click', () => {
      pageSpecs[name].offset = Math.max(0, info.offset - pageSpecs[name].limit);
      refresh();
    });
    controls.querySelector('[data-page-next]').addEventListener('click', () => {
      if (!info.hasMore) return;
      pageSpecs[name].offset = info.nextOffset;
      refresh();
    });
    container.append(controls);
  }

  function render() {
    if (!win.isConnected) return;
    renderedSnapshot = JSON.stringify(instance);
    host.querySelector('[data-summary]').innerHTML = `<div class="fb-process-summary"><h2>${escapeHtml(instance.definitionName)}</h2>
      <tf-chip status="${tone(instance.status)}">${escapeHtml(processStatusLabel(instance.status))}</tf-chip>
      <span>${escapeHtml(text('version_number', { version: instance.version }))}</span>
      ${instance.canSendMessage && instance.messageNames.length ? `<tf-button variant="secondary" icon="mail" data-send-instance>${escapeHtml(text('send_message'))}</tf-button>` : ''}
      ${instance.canCancel ? `<tf-button variant="danger" icon="x" data-cancel>${escapeHtml(text('cancel_instance'))}</tf-button>` : ''}</div>`;
    host.querySelector('[data-send-instance]')?.addEventListener('click', () => openProcessMessageSend({ Catch: { definitionId: instance.definitionId, instanceId, subscriptionId: null } }, instance.messageNames, () => refresh()));
    if (instance.terminalError) {
      const detail = document.createElement('section');
      detail.className = 'fb-process-work';
      detail.dataset.terminalError = '';
      detail.innerHTML = `<div><strong>${escapeHtml(text('terminal_business_error'))}</strong>
        <p>${escapeHtml(instance.terminalError.errorCode)} · ${escapeHtml(instance.terminalError.errorRef)}</p>
        <p>${escapeHtml(text('call_error_source'))}: ${escapeHtml(instance.terminalError.sourceNodeId)} · ${escapeHtml(instance.terminalError.sourceScopeId)}</p></div>`;
      host.querySelector('[data-summary]').append(detail);
    }
    host.querySelector('[data-cancel]')?.addEventListener('click', async () => {
      const approved = await customElements.get('tf-window').confirm({ title: text('cancel_instance'), message: text('cancel_hint'), danger: true, confirmLabel: text('cancel_instance') });
      if (approved && win.isConnected) await applyMutation('processInstanceCancelRequest', { instanceId, expectedRevision: instance.revision }, cancelCommand);
    });
    const work = host.querySelector('[data-work]');
    work.innerHTML = `<h3>${escapeHtml(text('human_work'))}</h3>${instance.userTasks.length ? '' : `<p>${escapeHtml(text('human_work_empty'))}</p>`}`;
    for (const task of instance.userTasks) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.innerHTML = `<div><strong>${escapeHtml(task.name)}</strong><p>${escapeHtml(text(`work_kind_${task.kind.toLowerCase()}`))} · ${escapeHtml(processStatusLabel(task.status))}</p><p>${escapeHtml(text('scope_id'))}: ${escapeHtml(task.scopeId)}</p></div>
        ${task.canComplete && task.status === 'Open' ? `<tf-button variant="primary" data-complete>${escapeHtml(text(task.kind === 'Verification' ? 'review_result' : task.kind === 'Manual' ? 'acknowledge_manual' : 'complete_work'))}</tf-button>` : ''}`;
      row.querySelector('[data-complete]')?.addEventListener('click', () => complete(task));
      work.appendChild(row);
    }
    renderPageControls(work, 'userTasks');
    const incidents = host.querySelector('[data-incidents]');
    incidents.innerHTML = instance.incidents.length ? `<h3>${escapeHtml(text('incidents'))}</h3>` : '';
    for (const incident of instance.incidents) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.innerHTML = `<div><strong>${escapeHtml(incident.nodeName || text('element_unavailable'))}</strong><p>${escapeHtml(processIncidentText(incident))}</p><p>${escapeHtml(text('scope_id'))}: ${escapeHtml(incident.scopeId)}</p></div>
        <tf-button variant="secondary" data-inspect-incident>${escapeHtml(text('inspect_incident'))}</tf-button>
        ${incident.canRetry && incident.jobId ? `<tf-button variant="secondary" icon="refresh" data-retry>${escapeHtml(text('retry'))}</tf-button>` : ''}`;
      row.querySelector('[data-inspect-incident]').addEventListener('click', () => { selectedIncidentId = incident.incidentId; refresh(); });
      row.querySelector('[data-retry]')?.addEventListener('click', async () => {
        selectedIncidentId = incident.incidentId;
        const approved = await customElements.get('tf-window').confirm({ title: text('retry'), message: text('retry_hint'), confirmLabel: text('retry') });
        if (approved && win.isConnected) await applyMutation('processJobRetryRequest', { instanceId, jobId: incident.jobId, expectedRevision: instance.revision }, retryCommand);
      });
      incidents.appendChild(row);
    }
    if (instance.selectedIncident) {
      const { incident, resolvedAtMs } = instance.selectedIncident;
      const detail = document.createElement('div');
      detail.className = 'fb-process-work';
      detail.dataset.selectedIncident = incident.incidentId;
      detail.innerHTML = `<div><strong>${escapeHtml(incident.nodeName || incident.nodeId)}</strong>
        <p>${escapeHtml(incident.code)} · ${escapeHtml(incident.message)}</p>
        ${resolvedAtMs == null ? '' : `<p>${escapeHtml(text('incident_resolved_at', { time: date(resolvedAtMs) }))}</p>`}</div>
        <tf-button variant="ghost" data-close-incident>${escapeHtml(text('close_detail'))}</tf-button>`;
      detail.querySelector('[data-close-incident]').addEventListener('click', () => { selectedIncidentId = null; refresh(); });
      incidents.append(detail);
    }
    renderPageControls(incidents, 'incidents');
    host.querySelector('[data-variables]').replaceChildren(jsonSection(text('current_variables'), instance.variables, false));
    host.querySelector('[data-timer-section]').hidden = !instance.timers?.length && !instance.pages.timers.total;
    renderTimers(host.querySelector('[data-timers]'), instance.timers || []);
    renderPageControls(host.querySelector('[data-timer-section]'), 'timers');
    const subscriptions = host.querySelector('[data-subscription-rows]');
    subscriptions.replaceChildren();
    for (const subscription of instance.subscriptions) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.dataset.subscriptionId = subscription.subscriptionId;
      const subject = subscription.kind === 'BoundaryEscalation'
        ? subscription.escalationCode || text('escalation_any_code')
        : subscription.signalName || subscription.messageName || subscription.errorCode || text('element_unavailable');
      row.innerHTML = `<div><strong>${escapeHtml(subscription.nodeName || subscription.nodeId)}</strong><p>${escapeHtml(subject)} · ${escapeHtml(text(`subscription_status_${subscription.status.toLowerCase()}`))}</p>
        ${subscription.kind === 'BoundaryEscalation' || subscription.kind === 'SignalCatch' ? '' : `<p>${escapeHtml(text('message_correlation_key'))}: ${escapeHtml(subscription.correlationKey || '')}</p>`}<p>${escapeHtml(subscription.subscriptionId)}</p></div>`;
      subscriptions.append(row);
    }
    renderPageControls(host.querySelector('[data-subscriptions]'), 'subscriptions');
    const races = host.querySelector('[data-race-rows]');
    races.replaceChildren();
    for (const race of instance.eventRaces) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.innerHTML = `<div><strong>${escapeHtml(race.gatewayName || race.gatewayNodeId)}</strong><p>${escapeHtml(text(`race_status_${race.status.toLowerCase()}`))}${race.winnerNodeId ? ` · ${escapeHtml(text('race_winning_element_id'))}: ${escapeHtml(race.winnerNodeId)}` : ''}</p></div>`;
      races.append(row);
    }
    renderPageControls(host.querySelector('[data-event-races]'), 'eventRaces');
    const outgoing = host.querySelector('[data-outgoing-rows]');
    outgoing.replaceChildren();
    for (const message of instance.outgoingMessages) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.dataset.messageId = message.messageId;
      row.innerHTML = `<div><strong>${escapeHtml(message.messageName)}</strong><p>${escapeHtml(text(`message_status_${message.status.toLowerCase()}`))} · ${escapeHtml(message.correlationKey)}</p><p>${escapeHtml(message.messageId)}</p></div>
        <tf-button variant="secondary" data-message-detail>${escapeHtml(text('message_detail'))}</tf-button>`;
      row.querySelector('[data-message-detail]').addEventListener('click', () => openProcessMessageDetail(message, refresh));
      outgoing.append(row);
    }
    renderPageControls(host.querySelector('[data-outgoing]'), 'outgoingMessages');
    const scopeRows = host.querySelector('[data-scope-rows]');
    scopeRows.replaceChildren();
    for (const scope of instance.scopes) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.dataset.scopeId = scope.scopeId;
      row.innerHTML = `<div><strong>${escapeHtml(scope.subprocessNodeName || text('root_scope'))}</strong>
        <p>${escapeHtml(processStatusLabel(scope.status))} · ${escapeHtml(text('scope_depth', { depth: scope.depth }))}</p>
        <p>${escapeHtml(text('scope_id'))}: ${escapeHtml(scope.scopeId)}</p>
        ${scope.terminalError ? `<p>${escapeHtml(text('terminal_business_error'))}: ${escapeHtml(scope.terminalError.errorCode)}</p>` : ''}
        ${scope.parentScopeId ? `<p>${escapeHtml(text('parent_scope_id'))}: ${escapeHtml(scope.parentScopeId)}</p>` : ''}</div>
        <tf-button variant="secondary" data-scope-inspect>${escapeHtml(text('scope_inspect'))}</tf-button>`;
      row.querySelector('[data-scope-inspect]').addEventListener('click', async () => {
        selectedScopeId = scope.scopeId;
        const generation = ++scopeReadGeneration;
        try {
          const result = await ApiBinary.one('processScopeGetRequest', { instanceId, scopeId: selectedScopeId });
          if (!win.isConnected || generation !== scopeReadGeneration || selectedScopeId !== result.scope.scopeId) return;
          const detail = host.querySelector('[data-scope-detail]');
          detail.replaceChildren(jsonSection(text('scope_variables'), result.variables, false));
          const nodes = document.createElement('p');
          nodes.textContent = `${text('scope_active_nodes')}: ${result.activeNodeIds.join(', ') || text('none')}`;
          detail.append(nodes);
        } catch (error) { if (win.isConnected && generation === scopeReadGeneration) showError(win, error); }
      });
      scopeRows.append(row);
    }
    renderPageControls(host.querySelector('[data-scopes]'), 'scopes');
    const callRows = host.querySelector('[data-call-rows]');
    callRows.replaceChildren();
    for (const call of instance.calls) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      if (call.Outgoing) {
        const { callNodeId, callNodeName, status, child } = call.Outgoing;
        row.innerHTML = `<div><strong>${escapeHtml(callNodeName)}</strong>
          <p>${escapeHtml(text('call_element_id'))}: ${escapeHtml(callNodeId)}</p>
          <p>${escapeHtml(text(`call_status_${status.toLowerCase()}`))}</p>
          ${child ? `<p>${escapeHtml(child.definitionName)} · ${escapeHtml(processStatusLabel(child.status))}</p>` : `<p>${escapeHtml(text('call_related_unavailable'))}</p>`}</div>
          ${child?.canOpen ? `<tf-button variant="secondary" data-open-related>${escapeHtml(text('call_open_child'))}</tf-button>` : ''}`;
        row.querySelector('[data-open-related]')?.addEventListener('click', () => openProcessInstance(child.instanceId));
      } else {
        const { parent } = call.Incoming;
        row.innerHTML = `<div><strong>${escapeHtml(text('call_incoming'))}</strong>
          ${parent ? `<p>${escapeHtml(parent.definitionName)} · ${escapeHtml(processStatusLabel(parent.status))}</p>` : `<p>${escapeHtml(text('call_related_unavailable'))}</p>`}</div>
          ${parent?.canOpen ? `<tf-button variant="secondary" data-open-related>${escapeHtml(text('call_open_parent'))}</tf-button>` : ''}`;
        row.querySelector('[data-open-related]')?.addEventListener('click', () => openProcessInstance(parent.instanceId));
      }
      callRows.append(row);
    }
    renderPageControls(host.querySelector('[data-calls]'), 'calls');
    const groups = host.querySelector('[data-repetition-group-rows]');
    groups.replaceChildren();
    for (const group of instance.repetitionGroups) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.dataset.repetitionGroupId = group.groupId;
      const count = group.total == null ? text('repeat_total_unknown') : String(group.total);
      row.innerHTML = `<div><strong>${escapeHtml(group.nodeName)}</strong>
        <p>${escapeHtml(text(`repeat_mode_${group.mode}`))} · ${escapeHtml(text(`repeat_status_${group.status}`))}</p>
        <p>${escapeHtml(text('repeat_progress', { completed: group.completed, created: group.createdCount, total: count }))}</p>
        <p>${escapeHtml(text('scope_id'))}: ${escapeHtml(group.scopeId)}</p></div>
        <tf-button variant="secondary" data-repeat-inspect>${escapeHtml(text('repeat_inspect'))}</tf-button>`;
      row.querySelector('[data-repeat-inspect]').addEventListener('click', () => {
        selectedRepetitionGroupId = group.groupId;
        selectedRepetitionOccurrenceId = null;
        selectedRepetitionValue = null;
        pageSpecs.repetitionOccurrences.offset = 0;
        refresh();
      });
      groups.append(row);
    }
    renderPageControls(host.querySelector('[data-repetition-groups]'), 'repetitionGroups');
    const occurrences = host.querySelector('[data-repetition-occurrence-rows]');
    occurrences.replaceChildren();
    for (const occurrence of instance.repetitionOccurrences) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.dataset.repetitionOccurrenceId = occurrence.occurrenceId;
      row.innerHTML = `<div><strong>${escapeHtml(text('repeat_ordinal', { ordinal: occurrence.ordinal + 1 }))}</strong>
        <p>${escapeHtml(text(`repeat_occurrence_status_${occurrence.status}`))}</p>
        <p>${escapeHtml(occurrence.occurrenceId)}</p></div>
        <tf-button variant="secondary" data-repeat-item>${escapeHtml(text('repeat_item'))}</tf-button>
        <tf-button variant="secondary" data-repeat-aggregate>${escapeHtml(text('repeat_aggregate'))}</tf-button>`;
      for (const [selector, kind] of [['[data-repeat-item]', 'item'], ['[data-repeat-aggregate]', 'aggregate']]) {
        row.querySelector(selector).addEventListener('click', () => {
          selectedRepetitionOccurrenceId = occurrence.occurrenceId;
          selectedRepetitionValue = kind;
          refresh();
        });
      }
      occurrences.append(row);
    }
    renderPageControls(host.querySelector('[data-repetition-occurrences]'), 'repetitionOccurrences');
    const detail = host.querySelector('[data-repetition-detail]');
    detail.replaceChildren();
    if (instance.selectedRepetitionOccurrence) {
      const selected = instance.selectedRepetitionOccurrence;
      const heading = document.createElement('p');
      heading.textContent = `${text('repeat_ordinal', { ordinal: selected.summary.ordinal + 1 })} · ${text(`repeat_${selected.valueKind}`)}`;
      detail.append(heading);
      if (selected.valueAvailable) detail.append(jsonSection(text(`repeat_${selected.valueKind}`), selected.value, false));
      else {
        const unavailable = document.createElement('p');
        unavailable.textContent = text('repeat_value_unavailable');
        detail.append(unavailable);
      }
      if (selected.summary.acceptedSourceEventId) {
        const source = document.createElement('p');
        source.textContent = `${text('repeat_accepted_source_event')}: ${selected.summary.acceptedSourceEventId}`;
        detail.append(source);
      }
      if (selected.acceptedOrigin) {
        const origin = document.createElement('p');
        origin.textContent = `${text('repeat_result_origin')}: ${text(`repeat_origin_${selected.acceptedOrigin.toLowerCase()}`)}`;
        detail.append(origin);
      }
    }
  }

  async function refresh() {
    if (refreshing || mutation || !win.isConnected) return;
    refreshing = true;
    const generation = ++readGeneration;
    try {
      const response = await requestInstance();
      if (!win.isConnected || generation !== readGeneration) return;
      host.querySelectorAll('[data-cancel], [data-complete], [data-retry]').forEach((control) => control.removeAttribute('disabled'));
      await acceptSnapshot(response.instance);
      if (historyEnded) await loadHistory();
    } catch (error) {
      if (!win.isConnected || generation !== readGeneration) return;
      available = false;
      host.querySelectorAll('[data-cancel], [data-complete], [data-retry]').forEach((control) => control.setAttribute('disabled', ''));
      syncWorkWindows();
      showError(win, error);
    }
    finally { refreshing = false; }
  }
  if (instance) render();
  else await refresh();
  await loadHistory();
  if (!win.isConnected) return win;
  host.querySelector('[data-more]').addEventListener('click', loadHistory);
  const timer = setInterval(refresh, 3000);
  win.addEventListener('closed', () => {
    clearInterval(timer);
    readGeneration += 1;
    for (const workWindow of workWindows) workWindow.close();
  }, { once: true });
  return win;
}

export function openProcessRun(definition, versions, startCatalog, selectedStart) {
  const selection = document.createElement('div');
  const manualStarts = startCatalog.filter((entry) => entry.trigger?.Start?.canStart);
  selection.innerHTML = `<tf-select data-version label="${escapeAttr(text('published_version'))}">${versions.map((version) => `<option value="${version.version}">${escapeHtml(text('version_number', { version: version.version }))}</option>`).join('')}</tf-select>
    <tf-select data-start wrap-selected label="${escapeAttr(text('start_entry'))}"><option value="">${escapeHtml(text('start_entry'))}</option>${manualStarts.map((entry, index) => `<option value="${index}">${escapeHtml(entry.processName || entry.processId)} · ${escapeHtml(entry.startNodeName || entry.startNodeId)}</option>`).join('')}</tf-select>`;
  const variables = jsonSection(text('initial_variables'), definition.model.variables);
  const selectedIndex = manualStarts.findIndex((entry) => entry.processId === selectedStart?.processId
    && entry.startNodeId === selectedStart?.startNodeId);
  selection.querySelector('[data-start]').value = selectedIndex < 0 ? '' : String(selectedIndex);
  const command = processCommand();
  return openFormWindow({ title: text('run'), icon: 'play', subject: definition.name,
    note: { text: text('run_hint') }, sections: [selection, variables], submitLabel: text('run'),
    validate: () => validateJson(variables, true),
    canSubmit: () => versions.length > 0 && manualStarts.length > 0 && !definition.archived
      && selection.querySelector('[data-start]').value !== '',
    collect: () => {
      const value = selection.querySelector('[data-start]').value;
      const selected = value === '' ? undefined : manualStarts[Number(value)];
      if (!selected) throw new Error(text('start_entry_changed'));
      return { definitionId: definition.definitionId,
        version: Number(selection.querySelector('[data-version]').value),
        processId: selected.processId, startNodeId: selected.startNodeId,
        variables: variables.jsonValue };
    },
    onSubmit: async (payload) => {
      const response = await ApiBinary.one('processInstanceStartRequest', command(payload));
      await openProcessInstance(response.instance.instanceId, response.instance);
      return { message: text('started') };
    },
  });
}

export function openProcessVariables(value, onSave) {
  const section = jsonSection(text('initial_variables'), value);
  return openFormWindow({ title: text('variables'), icon: 'code', sections: [section], submitLabel: text('apply'),
    validate: () => validateJson(section, true), collect: () => section.jsonValue,
    onSubmit: async (values) => { onSave(values); return { message: text('variables_updated') }; },
  });
}

export function openProcessDeclarations(model, readOnly, onSave) {
  const section = document.createElement('section');
  section.className = 'fb-process-declarations';
  section.innerHTML = `<tf-textarea data-declaration-namespace label="${escapeAttr(text('target_namespace'))}" value="${escapeAttr(model.targetNamespace || '')}" autogrow rows="2" ${readOnly ? 'disabled' : ''}></tf-textarea>
    <h3>${escapeHtml(text('message_declarations'))}</h3><div data-message-rows></div>
    ${readOnly ? '' : `<tf-button variant="secondary" data-add-message>${escapeHtml(text('add_message_declaration'))}</tf-button>`}
    <h3>${escapeHtml(text('error_declarations'))}</h3><div data-error-rows></div>
    ${readOnly ? '' : `<tf-button variant="secondary" data-add-error>${escapeHtml(text('add_error_declaration'))}</tf-button>`}
    <h3>${escapeHtml(text('escalation_declarations'))}</h3><div data-escalation-rows></div>
    ${readOnly ? '' : `<tf-button variant="secondary" data-add-escalation>${escapeHtml(text('add_escalation_declaration'))}</tf-button>`}
    <h3>${escapeHtml(text('signal_declarations'))}</h3><div data-signal-rows></div>
    ${readOnly ? '' : `<tf-button variant="secondary" data-add-signal>${escapeHtml(text('add_signal_declaration'))}</tf-button>`}
    <tf-alert data-declaration-error tone="danger" hidden></tf-alert>`;
  const addRow = (kind, declaration) => {
    const row = document.createElement('div');
    row.className = 'fb-declaration-row';
    row.dataset.declarationKind = kind;
    const message = kind === 'message';
    const escalation = kind === 'escalation';
    const signal = kind === 'signal';
    row.innerHTML = `<tf-textarea data-declaration-id label="${escapeAttr(text(message ? 'message_declaration_id' : escalation ? 'escalation_declaration_id' : signal ? 'signal_declaration_id' : 'error_declaration_id'))}" value="${escapeAttr(message ? declaration.messageId : escalation ? declaration.escalationId : signal ? declaration.signalId : declaration.errorId)}" autogrow rows="2" ${readOnly ? 'disabled' : ''}></tf-textarea>
      <tf-textarea data-declaration-name label="${escapeAttr(text('declaration_name'))}" value="${escapeAttr(declaration.name)}" autogrow rows="2" ${readOnly ? 'disabled' : ''}></tf-textarea>
      ${message || signal ? '' : `<tf-textarea data-declaration-code label="${escapeAttr(text(escalation ? 'escalation_code' : 'error_code'))}" value="${escapeAttr(escalation ? declaration.escalationCode : declaration.errorCode)}" autogrow rows="2" ${readOnly ? 'disabled' : ''}></tf-textarea>`}
      ${readOnly ? '' : `<tf-button variant="ghost" wrap data-remove-declaration>${escapeHtml(text('remove_declaration'))}</tf-button>`}`;
    section.querySelector(`[data-${kind}-rows]`).append(row);
  };
  (model.messages || []).forEach((item) => addRow('message', item));
  (model.errors || []).forEach((item) => addRow('error', item));
  (model.escalations || []).forEach((item) => addRow('escalation', item));
  (model.signals || []).forEach((item) => addRow('signal', item));
  section.addEventListener('click', (event) => {
    if (readOnly) return;
    if (event.target.closest('[data-add-message]')) addRow('message', { messageId: `Message_${crypto.randomUUID().replaceAll('-', '_')}`, name: '' });
    else if (event.target.closest('[data-add-error]')) addRow('error', { errorId: `Error_${crypto.randomUUID().replaceAll('-', '_')}`, name: '', errorCode: '' });
    else if (event.target.closest('[data-add-escalation]')) addRow('escalation', { escalationId: `Escalation_${crypto.randomUUID().replaceAll('-', '_')}`, name: '', escalationCode: '' });
    else if (event.target.closest('[data-add-signal]')) addRow('signal', { signalId: `Signal_${crypto.randomUUID().replaceAll('-', '_')}`, name: '' });
    else event.target.closest('[data-remove-declaration]')?.closest('.fb-declaration-row')?.remove();
  });
  const collect = () => {
    const rows = (kind) => Array.from(section.querySelectorAll(`[data-declaration-kind="${kind}"]`)).map((row) => {
      const id = row.querySelector('[data-declaration-id]').value;
      const name = row.querySelector('[data-declaration-name]').value;
      return kind === 'message' ? { messageId: id, name }
        : kind === 'escalation' ? { escalationId: id, name, escalationCode: row.querySelector('[data-declaration-code]').value }
          : kind === 'signal' ? { signalId: id, namespaceUri: section.querySelector('[data-declaration-namespace]').value, name }
          : { errorId: id, name, errorCode: row.querySelector('[data-declaration-code]').value };
    });
    return { messages: rows('message'), errors: rows('error'), escalations: rows('escalation'), signals: rows('signal'),
      targetNamespace: section.querySelector('[data-declaration-namespace]').value || null };
  };
  return openFormWindow({ title: text('declarations'), icon: 'mail', width: 720, sections: [section],
    submitLabel: text('apply'), canSubmit: () => !readOnly, collect,
    onSubmit: async (declarations) => { onSave(declarations); return { message: text('declarations_updated') }; },
  });
}

export function openProcessModeling(model, path, processId, readOnly, onSave) {
  const draft = structuredClone(model);
  const body = processBody(draft, path, processId);
  const hadModeling = body.modeling != null;
  body.modeling ??= { laneSets: [], dataObjects: [], dataObjectReferences: [], textAnnotations: [], associations: [], dataStoreReferences: [] };
  const modeling = body.modeling;
  modeling.dataStoreReferences ??= [];
  draft.dataStores ??= [];
  body.diagram.modelingShapes ??= [];
  body.diagram.modelingEdges ??= [];
  const section = document.createElement('section');
  const id = (prefix) => `${prefix}_${crypto.randomUUID().replaceAll('-', '_')}`;
  const bodyShapes = body.diagram.modelingShapes;
  const bodyEdges = body.diagram.modelingEdges;
  const shape = (elementId) => bodyShapes.find((item) => item.elementId === elementId);
  const nextShape = (elementId, x, y, width, height) => ({
    diId: id('Shape'), elementId, x, y, width, height,
  });
  const laneRows = () => {
    const rows = [];
    const visit = (sets, depth) => {
      for (const set of sets) for (const lane of set.lanes) {
        rows.push({ lane, set, depth });
        visit(lane.childLaneSets || [], depth + 1);
      }
    };
    visit(modeling.laneSets, 0);
    return rows;
  };
  const allBodies = [draft, ...(draft.additionalProcesses || [])];
  const nodesInBody = (selectedBody) => selectedBody.nodes.flatMap((node) => [node,
    ...(node.kind?.SubProcess ? nodesInBody(node.kind.SubProcess.body) : [])]);
  const storeReferenced = (storeId) => {
    const pending = [...allBodies];
    while (pending.length) {
      const selectedBody = pending.pop();
      if ((selectedBody.modeling?.dataStoreReferences || []).some((reference) => reference.dataStoreRef === storeId)) return true;
      pending.push(...selectedBody.nodes.filter((node) => node.kind?.SubProcess).map((node) => node.kind.SubProcess.body));
    }
    return false;
  };
  const namespaceUri = draft.targetNamespace || 'https://tentaflow.app/bpmn/1';
  const geometry = (owner, item) => {
    const bounds = owner.find((entry) => entry.elementId === item.id);
    if (!bounds) return readOnly ? '' : `<tf-button variant="secondary" data-place="${escapeAttr(item.id)}">${escapeHtml(text('modeling_place'))}</tf-button>`;
    return `<div data-geometry="${escapeAttr(item.id)}">${[['x', 'modeling_x'], ['y', 'modeling_y'],
      ['width', 'modeling_width'], ['height', 'modeling_height']].map(([key, label]) =>
      `<tf-input type="number" step="1" data-geometry-field="${key}" value="${bounds[key]}" label="${escapeAttr(text(label))}" ${readOnly ? 'disabled' : ''}></tf-input>`).join('')}</div>`;
  };
  const optionList = (items, selected, blank = '') => `<option value="">${escapeHtml(blank)}</option>${items.map(([value, label]) =>
    `<option value="${escapeAttr(value)}" ${value === selected ? 'selected' : ''}>${escapeHtml(label)}</option>`).join('')}`;
  const edgeGeometry = (edges, item) => {
    const edge = edges.find((entry) => entry.elementId === item.id);
    if (!edge) return '';
    return `<div data-edge-geometry="${escapeAttr(item.id)}">${edge.waypoints.map((point, index) =>
      `<div data-point-index="${index}">${[['x', 'modeling_x'], ['y', 'modeling_y']].map(([field, label]) =>
        `<tf-input type="number" step="1" data-point-field="${field}" value="${point[field]}" label="${escapeAttr(`${text(label)} ${index + 1}`)}" ${readOnly ? 'disabled' : ''}></tf-input>`).join('')}</div>`).join('')}</div>`;
  };
  const render = () => {
    const lanes = laneRows();
    const refs = modeling.dataObjectReferences;
    const annotationIds = modeling.textAnnotations.map((item) => [item.id, item.text || item.id]);
    const endpoints = [...body.nodes.map((item) => [item.id, item.name || item.id]),
      ...refs.map((item) => [item.id, item.name ?? item.id]),
      ...modeling.dataStoreReferences.map((item) => [item.id, item.name ?? item.id]), ...annotationIds];
    const variableOptions = Object.keys(body.variables).map((key) => [key, key]);
    const collaboration = draft.collaboration;
    const participantShapes = collaboration?.diagram.modelingShapes || [];
    const participantOptions = (collaboration?.participants || []).map((item) => [item.id, item.name ?? item.id]);
    const messageEndpoints = [...participantOptions,
      ...allBodies.flatMap((item) => nodesInBody(item).map((node) => [node.id, node.name || node.id]))];
    section.innerHTML = `<p>${escapeHtml(text('modeling_hint'))}</p>
      <h3>${escapeHtml(text('modeling_lanes'))}</h3>
      <div data-modeling-lanes>${lanes.map(({ lane, depth }) => `<div data-modeling-kind="lane" data-modeling-id="${escapeAttr(lane.id)}">
        <tf-input data-modeling-field="name" value="${escapeAttr(lane.name ?? '')}" label="${escapeAttr(`${text('modeling_lane')} ${depth + 1}`)}" ${readOnly ? 'disabled' : ''}></tf-input>
        ${geometry(bodyShapes, lane)}
        <tf-select data-modeling-node="${escapeAttr(lane.id)}" wrap-selected label="${escapeAttr(text('modeling_assign_node'))}" ${readOnly ? 'disabled' : ''}>${optionList(body.nodes.map((node) => [node.id, node.name || node.id]), '', text('modeling_choose'))}</tf-select>
        ${readOnly ? '' : `<tf-button variant="secondary" data-assign-node="${escapeAttr(lane.id)}">${escapeHtml(text('modeling_assign_node'))}</tf-button>
          ${depth < 3 ? `<tf-button variant="secondary" data-add-child-lane="${escapeAttr(lane.id)}">${escapeHtml(text('modeling_add_child_lane'))}</tf-button>` : ''}
          <tf-button variant="danger" data-remove-modeling="${escapeAttr(lane.id)}">${escapeHtml(text('remove_declaration'))}</tf-button>`}
        <div>${lane.flowNodeRefs.map((nodeId) => `<tf-chip status="info">${escapeHtml(body.nodes.find((node) => node.id === nodeId)?.name || nodeId)}</tf-chip>${readOnly ? '' : `<tf-button variant="ghost" data-remove-node="${escapeAttr(lane.id)}" data-node-id="${escapeAttr(nodeId)}">${escapeHtml(text('remove_declaration'))}</tf-button>`}`).join('')}</div>
      </div>`).join('')}</div>
      ${readOnly ? '' : `<tf-button variant="secondary" data-add-modeling="lane">${escapeHtml(text('modeling_add_lane'))}</tf-button>`}
      <h3>${escapeHtml(text('modeling_stores'))}</h3>
      <div data-modeling-stores>${draft.dataStores.map((store) => `<div data-modeling-kind="store" data-modeling-id="${escapeAttr(store.id)}">
        <tf-input data-modeling-field="name" value="${escapeAttr(store.name ?? '')}" label="${escapeAttr(text('modeling_store'))}" ${readOnly ? 'disabled' : ''}></tf-input>
        <tf-input type="number" step="1" min="0" max="9007199254740991" data-modeling-field="capacity" value="${store.capacity ?? ''}" label="${escapeAttr(text('modeling_store_capacity'))}" ${readOnly ? 'disabled' : ''}></tf-input>
        <tf-select data-modeling-field="isUnlimited" label="${escapeAttr(text('modeling_store_unlimited'))}" ${readOnly ? 'disabled' : ''}>${optionList([['true', I18n.t('common.yes')], ['false', I18n.t('common.no')]], store.isUnlimited == null ? '' : String(store.isUnlimited), text('modeling_descriptive'))}</tf-select>
        ${readOnly || storeReferenced(store.id) ? '' : `<tf-button variant="danger" data-remove-modeling="${escapeAttr(store.id)}">${escapeHtml(text('remove_declaration'))}</tf-button>`}
      </div>`).join('')}</div>
      ${readOnly ? '' : `<tf-button variant="secondary" data-add-modeling="store">${escapeHtml(text('modeling_add_store'))}</tf-button>`}
      <h3>${escapeHtml(text('modeling_store_references'))}</h3>
      <div data-modeling-store-references>${modeling.dataStoreReferences.map((reference) => `<div data-modeling-kind="storeReference" data-modeling-id="${escapeAttr(reference.id)}">
        <tf-input data-modeling-field="name" value="${escapeAttr(reference.name ?? '')}" label="${escapeAttr(text('modeling_store_reference'))}" ${readOnly ? 'disabled' : ''}></tf-input>
        <tf-select data-modeling-field="dataStoreRef" wrap-selected label="${escapeAttr(text('modeling_store'))}" ${readOnly ? 'disabled' : ''}>${optionList(draft.dataStores.map((store) => [store.id, store.name ?? store.id]), reference.dataStoreRef, text('modeling_choose'))}</tf-select>
        ${geometry(bodyShapes, reference)}
        ${readOnly ? '' : `<tf-button variant="danger" data-remove-modeling="${escapeAttr(reference.id)}">${escapeHtml(text('remove_declaration'))}</tf-button>`}
      </div>`).join('')}</div>
      ${readOnly || !draft.dataStores.length ? '' : `<tf-button variant="secondary" data-add-modeling="storeReference">${escapeHtml(text('modeling_add_store_reference'))}</tf-button>`}
      <h3>${escapeHtml(text('modeling_data'))}</h3>
      <div data-modeling-data>${refs.map((reference) => {
        const object = modeling.dataObjects.find((item) => item.id === reference.dataObjectRef);
        return `<div data-modeling-kind="reference" data-modeling-id="${escapeAttr(reference.id)}">
          <tf-input data-modeling-field="objectName" value="${escapeAttr(object?.name ?? '')}" label="${escapeAttr(text('modeling_data_object'))}" ${readOnly ? 'disabled' : ''}></tf-input>
          <tf-input data-modeling-field="name" value="${escapeAttr(reference.name ?? '')}" label="${escapeAttr(text('modeling_data_reference'))}" ${readOnly ? 'disabled' : ''}></tf-input>
          <tf-select data-modeling-field="variableBindingKey" wrap-selected label="${escapeAttr(text('modeling_binding'))}" ${readOnly ? 'disabled' : ''}>${optionList(variableOptions, reference.variableBindingKey, text('modeling_descriptive'))}</tf-select>
          ${geometry(bodyShapes, reference)}
          ${readOnly ? '' : `<tf-button variant="danger" data-remove-modeling="${escapeAttr(reference.id)}">${escapeHtml(text('remove_declaration'))}</tf-button>`}
        </div>`;
      }).join('')}</div>
      ${readOnly ? '' : `<tf-button variant="secondary" data-add-modeling="reference">${escapeHtml(text('modeling_add_data'))}</tf-button>`}
      <h3>${escapeHtml(text('modeling_annotations'))}</h3>
      <div data-modeling-annotations>${modeling.textAnnotations.map((annotation) => `<div data-modeling-kind="annotation" data-modeling-id="${escapeAttr(annotation.id)}">
        <tf-textarea data-modeling-field="text" value="${escapeAttr(annotation.text)}" label="${escapeAttr(text('modeling_annotation'))}" autogrow rows="3" ${readOnly ? 'disabled' : ''}></tf-textarea>
        ${geometry(bodyShapes, annotation)}
        ${readOnly ? '' : `<tf-button variant="danger" data-remove-modeling="${escapeAttr(annotation.id)}">${escapeHtml(text('remove_declaration'))}</tf-button>`}
      </div>`).join('')}</div>
      ${readOnly ? '' : `<tf-button variant="secondary" data-add-modeling="annotation">${escapeHtml(text('modeling_add_annotation'))}</tf-button>`}
      <h3>${escapeHtml(text('modeling_associations'))}</h3>
      <div data-modeling-associations>${modeling.associations.map((association) => `<div data-modeling-kind="association" data-modeling-id="${escapeAttr(association.id)}">
        <tf-select data-modeling-field="sourceRef" wrap-selected label="${escapeAttr(text('modeling_source'))}" ${readOnly ? 'disabled' : ''}>${optionList(endpoints, association.sourceRef)}</tf-select>
        <tf-select data-modeling-field="targetRef" wrap-selected label="${escapeAttr(text('modeling_target'))}" ${readOnly ? 'disabled' : ''}>${optionList(endpoints, association.targetRef)}</tf-select>
        ${edgeGeometry(bodyEdges, association)}
        ${readOnly ? '' : `<tf-button variant="danger" data-remove-modeling="${escapeAttr(association.id)}">${escapeHtml(text('remove_declaration'))}</tf-button>`}
      </div>`).join('')}</div>
      ${readOnly ? '' : `<tf-button variant="secondary" data-add-modeling="association">${escapeHtml(text('modeling_add_association'))}</tf-button>`}
      ${path.length ? '' : `<h3>${escapeHtml(text('modeling_pools'))}</h3>
      <div data-modeling-participants>${(collaboration?.participants || []).map((participant) => `<div data-modeling-kind="participant" data-modeling-id="${escapeAttr(participant.id)}">
        <tf-input data-modeling-field="name" value="${escapeAttr(participant.name ?? '')}" label="${escapeAttr(text('modeling_pool'))}" ${readOnly ? 'disabled' : ''}></tf-input>
        <tf-select data-modeling-field="processRef" wrap-selected label="${escapeAttr(text('modeling_pool_body'))}" ${readOnly ? 'disabled' : ''}>${optionList(allBodies.map((item) => [item.processId, item.processName || item.processId]), participant.processRef?.processId, text('modeling_black_box'))}</tf-select>
        ${geometry(participantShapes, participant)}
        ${readOnly ? '' : `<tf-button variant="danger" data-remove-modeling="${escapeAttr(participant.id)}">${escapeHtml(text('remove_declaration'))}</tf-button>`}
      </div>`).join('')}</div>
      ${readOnly ? '' : `<tf-button variant="secondary" data-add-modeling="participant">${escapeHtml(text('modeling_add_pool'))}</tf-button>`}
      <h3>${escapeHtml(text('modeling_message_flows'))}</h3>
      <div data-modeling-message-flows>${(collaboration?.messageFlows || []).map((flow) => `<div data-modeling-kind="messageFlow" data-modeling-id="${escapeAttr(flow.id)}">
        <tf-select data-modeling-field="sourceRef" wrap-selected label="${escapeAttr(text('modeling_source'))}" ${readOnly ? 'disabled' : ''}>${optionList(messageEndpoints, flow.sourceRef)}</tf-select>
        <tf-select data-modeling-field="targetRef" wrap-selected label="${escapeAttr(text('modeling_target'))}" ${readOnly ? 'disabled' : ''}>${optionList(messageEndpoints, flow.targetRef)}</tf-select>
        <tf-select data-modeling-field="messageRef" wrap-selected label="${escapeAttr(text('message_reference'))}" ${readOnly ? 'disabled' : ''}>${optionList((draft.messages || []).map((message) => [message.messageId, message.name]), flow.messageRef, text('modeling_descriptive'))}</tf-select>
        ${edgeGeometry(collaboration.diagram.modelingEdges, flow)}
        ${readOnly ? '' : `<tf-button variant="danger" data-remove-modeling="${escapeAttr(flow.id)}">${escapeHtml(text('remove_declaration'))}</tf-button>`}
      </div>`).join('')}</div>
      ${readOnly ? '' : `<tf-button variant="secondary" data-add-modeling="messageFlow">${escapeHtml(text('modeling_add_message_flow'))}</tf-button>`}`}`;
    section.querySelectorAll('tf-select[data-modeling-field], tf-select[data-modeling-node]').forEach((select) => {
      const options = [...select.querySelectorAll('option')].map((option) => ({ value: option.value, label: option.textContent }));
      const selected = select.querySelector('option[selected]')?.value ?? '';
      select.setOptions(options, selected);
    });
  };
  const locate = (kind, itemId) => kind === 'lane' ? laneRows().find(({ lane }) => lane.id === itemId)?.lane
    : kind === 'store' ? draft.dataStores.find((item) => item.id === itemId)
      : kind === 'storeReference' ? modeling.dataStoreReferences.find((item) => item.id === itemId)
    : kind === 'reference' ? modeling.dataObjectReferences.find((item) => item.id === itemId)
      : kind === 'annotation' ? modeling.textAnnotations.find((item) => item.id === itemId)
        : kind === 'association' ? modeling.associations.find((item) => item.id === itemId)
          : kind === 'participant' ? draft.collaboration?.participants.find((item) => item.id === itemId)
            : draft.collaboration?.messageFlows.find((item) => item.id === itemId);
  const removeLinks = (itemId) => {
    modeling.associations = modeling.associations.filter((item) => item.sourceRef !== itemId && item.targetRef !== itemId && item.id !== itemId);
    const linked = new Set(modeling.associations.map((item) => item.id));
    for (let index = bodyEdges.length - 1; index >= 0; index -= 1) {
      if (!linked.has(bodyEdges[index].elementId)) bodyEdges.splice(index, 1);
    }
  };
  section.addEventListener('change', (event) => {
    if (readOnly) return;
    const row = event.target.closest('[data-modeling-kind]');
    const field = event.target.dataset.modelingField;
    if (!row || !field) return;
    const item = locate(row.dataset.modelingKind, row.dataset.modelingId);
    if (!item) return;
    if (field === 'objectName') {
      const object = modeling.dataObjects.find((candidate) => candidate.id === item.dataObjectRef);
      if (object) object.name = event.target.value;
    } else if (field === 'variableBindingKey') {
      if (event.target.value) item.variableBindingKey = event.target.value;
      else delete item.variableBindingKey;
    } else if (field === 'capacity') {
      if (event.target.value === '') delete item.capacity;
      else item.capacity = Number(event.target.value);
    } else if (field === 'isUnlimited') {
      if (event.target.value === '') delete item.isUnlimited;
      else item.isUnlimited = event.target.value === 'true';
    } else if (field === 'processRef') {
      if (event.target.value) item.processRef = { namespaceUri, processId: event.target.value };
      else delete item.processRef;
    } else if (field === 'messageRef') {
      if (event.target.value) item.messageRef = event.target.value;
      else delete item.messageRef;
    } else {
      item[field] = event.target.value;
      if (field === 'dataStoreRef') render();
    }
  });
  section.addEventListener('change', (event) => {
    const container = event.target.closest('[data-geometry]');
    const field = event.target.dataset.geometryField;
    if (!container || !field || readOnly) return;
    const owner = draft.collaboration?.participants.some((item) => item.id === container.dataset.geometry)
      ? draft.collaboration.diagram.modelingShapes : bodyShapes;
    const bounds = owner.find((item) => item.elementId === container.dataset.geometry);
    if (bounds && Number.isFinite(Number(event.target.value))) bounds[field] = Number(event.target.value);
  });
  section.addEventListener('change', (event) => {
    const container = event.target.closest('[data-edge-geometry]');
    const point = event.target.closest('[data-point-index]');
    const field = event.target.dataset.pointField;
    if (!container || !point || !field || readOnly) return;
    const edges = draft.collaboration?.messageFlows.some((item) => item.id === container.dataset.edgeGeometry)
      ? draft.collaboration.diagram.modelingEdges : bodyEdges;
    const edge = edges.find((item) => item.elementId === container.dataset.edgeGeometry);
    const coordinate = Number(event.target.value);
    if (edge && Number.isFinite(coordinate)) edge.waypoints[Number(point.dataset.pointIndex)][field] = coordinate;
  });
  section.addEventListener('click', (event) => {
    if (readOnly) return;
    const button = event.target.closest('tf-button');
    if (!button || !section.contains(button)) return;
    const kind = button.dataset.addModeling;
    if (kind === 'lane') {
      if (!modeling.laneSets.length) modeling.laneSets.push({ id: id('LaneSet'), lanes: [] });
      const laneId = id('Lane');
      modeling.laneSets[0].lanes.push({ id: laneId, flowNodeRefs: [], childLaneSets: [] });
      bodyShapes.push(nextShape(laneId, 60, 60, 720, 360));
    } else if (button.dataset.addChildLane) {
      const parent = locate('lane', button.dataset.addChildLane);
      if (parent) {
        const laneId = id('Lane');
        parent.childLaneSets.push({ id: id('LaneSet'), lanes: [{ id: laneId, flowNodeRefs: [], childLaneSets: [] }] });
        bodyShapes.push(nextShape(laneId, 100, 100, 520, 220));
      }
    } else if (kind === 'store') {
      draft.dataStores.push({ id: id('DataStore') });
    } else if (kind === 'storeReference') {
      if (!draft.dataStores.length) return;
      const referenceId = id('DataStoreRef');
      modeling.dataStoreReferences.push({ id: referenceId, dataStoreRef: '' });
      bodyShapes.push(nextShape(referenceId, 300, 460, 160, 90));
    } else if (kind === 'reference') {
      const objectId = id('DataObject'), referenceId = id('DataObjectRef');
      modeling.dataObjects.push({ id: objectId });
      modeling.dataObjectReferences.push({ id: referenceId, dataObjectRef: objectId });
      bodyShapes.push(nextShape(referenceId, 300, 340, 150, 90));
    } else if (kind === 'annotation') {
      const annotationId = id('Annotation');
      modeling.textAnnotations.push({ id: annotationId, text: '' });
      bodyShapes.push(nextShape(annotationId, 470, 340, 190, 100));
    } else if (kind === 'association' || kind === 'messageFlow') {
      if (kind === 'messageFlow' && !draft.collaboration) return;
      const endpoints = kind === 'association'
        ? [...body.nodes.map((item) => item.id), ...modeling.dataObjectReferences.map((item) => item.id),
          ...modeling.dataStoreReferences.map((item) => item.id), ...modeling.textAnnotations.map((item) => item.id)]
        : [...(draft.collaboration?.participants || []).map((item) => item.id), ...allBodies.flatMap((item) => nodesInBody(item).map((node) => node.id))];
      if (endpoints.length < 2) return;
      const flowId = id(kind === 'association' ? 'Association' : 'MessageFlow');
      const flow = { id: flowId, sourceRef: endpoints[0], targetRef: endpoints[1] };
      const owner = kind === 'association' ? modeling.associations : draft.collaboration.messageFlows;
      const edges = kind === 'association' ? bodyEdges : draft.collaboration.diagram.modelingEdges;
      owner.push(flow);
      edges.push({ diId: id('Edge'), elementId: flowId,
        waypoints: [{ x: 180, y: 200 }, { x: 400, y: 200 }] });
    } else if (kind === 'participant') {
      draft.collaboration ??= { id: id('Collaboration'), participants: [], messageFlows: [],
        diagram: { shapes: [], edges: [], modelingShapes: [], modelingEdges: [] } };
      const participantId = id('Participant');
      draft.collaboration.participants.push({ id: participantId });
      draft.collaboration.diagram.modelingShapes.push(nextShape(participantId, 20, 20, 900, 520));
    } else if (button.dataset.assignNode) {
      const lane = locate('lane', button.dataset.assignNode);
      const nodeId = section.querySelector(`[data-modeling-node="${CSS.escape(button.dataset.assignNode)}"]`)?.value;
      if (lane && nodeId && !lane.flowNodeRefs.includes(nodeId)) lane.flowNodeRefs.push(nodeId);
    } else if (button.dataset.removeNode) {
      const lane = locate('lane', button.dataset.removeNode);
      if (lane) lane.flowNodeRefs = lane.flowNodeRefs.filter((value) => value !== button.dataset.nodeId);
    } else if (button.dataset.place) {
      const owner = draft.collaboration?.participants.some((item) => item.id === button.dataset.place)
        ? draft.collaboration.diagram.modelingShapes : bodyShapes;
      owner.push(nextShape(button.dataset.place, 100, 100, 240, 120));
    } else if (button.dataset.removeModeling) {
      const itemId = button.dataset.removeModeling;
      if (draft.dataStores.some((item) => item.id === itemId) && storeReferenced(itemId)) return;
      draft.dataStores = draft.dataStores.filter((item) => item.id !== itemId);
      const lane = laneRows().find(({ lane: item }) => item.id === itemId);
      const removed = new Set([itemId]);
      if (lane) {
        const collect = (item) => {
          removed.add(item.id);
          for (const set of item.childLaneSets || []) for (const child of set.lanes) collect(child);
        };
        collect(lane.lane);
        lane.set.lanes = lane.set.lanes.filter((item) => item.id !== itemId);
      }
      const reference = modeling.dataObjectReferences.find((item) => item.id === itemId);
      modeling.dataObjectReferences = modeling.dataObjectReferences.filter((item) => item.id !== itemId);
      modeling.dataStoreReferences = modeling.dataStoreReferences.filter((item) => item.id !== itemId);
      if (reference && !modeling.dataObjectReferences.some((item) => item.dataObjectRef === reference.dataObjectRef))
        modeling.dataObjects = modeling.dataObjects.filter((item) => item.id !== reference.dataObjectRef);
      modeling.textAnnotations = modeling.textAnnotations.filter((item) => item.id !== itemId);
      for (const elementId of removed) removeLinks(elementId);
      for (let index = bodyShapes.length - 1; index >= 0; index -= 1) {
        if (removed.has(bodyShapes[index].elementId)) bodyShapes.splice(index, 1);
      }
      if (draft.collaboration) {
        draft.collaboration.participants = draft.collaboration.participants.filter((item) => item.id !== itemId);
        draft.collaboration.messageFlows = draft.collaboration.messageFlows.filter((item) => item.id !== itemId
          && item.sourceRef !== itemId && item.targetRef !== itemId);
        draft.collaboration.diagram.modelingShapes = draft.collaboration.diagram.modelingShapes.filter((item) => item.elementId !== itemId);
        const ids = new Set(draft.collaboration.messageFlows.map((item) => item.id));
        draft.collaboration.diagram.modelingEdges = draft.collaboration.diagram.modelingEdges.filter((item) => ids.has(item.elementId));
      }
    } else return;
    render();
  });
  const win = openFormWindow({ title: text('modeling_title'), icon: 'layers', width: 820,
    sections: [section], submitLabel: text('apply'), canSubmit: () => !readOnly,
    validate: () => {
      const bounds = [...(body.diagram.modelingShapes || []), ...(draft.collaboration?.diagram.modelingShapes || [])];
      const edges = [...(body.diagram.modelingEdges || []), ...(draft.collaboration?.diagram.modelingEdges || [])];
      return bounds.every((item) => [item.x, item.y, item.width, item.height].every(Number.isFinite)
        && item.width > 0 && item.height > 0)
        && draft.dataStores.every((store) => store.capacity == null
          || (Number.isSafeInteger(store.capacity) && store.capacity >= 0))
        && modeling.dataStoreReferences.every((reference) => draft.dataStores.some((store) => store.id === reference.dataStoreRef))
        && edges.every((edge) => edge.waypoints.length >= 2 && edge.waypoints.every((point) =>
          Number.isFinite(point.x) && Number.isFinite(point.y)));
    },
    collect: () => ({ modeling: hadModeling || modeling.laneSets.length || modeling.dataObjects.length
      || modeling.dataObjectReferences.length || modeling.dataStoreReferences.length
      || modeling.textAnnotations.length || modeling.associations.length
      ? body.modeling : null,
      modelingShapes: body.diagram.modelingShapes || [], modelingEdges: body.diagram.modelingEdges || [],
      collaboration: draft.collaboration || null, dataStores: draft.dataStores }),
    onSubmit: async (value) => { onSave(value); return { message: text('modeling_updated') }; },
  });
  render();
  return win;
}

export function openProcessMessageSend(target, names, onSent = () => {}) {
  const section = document.createElement('section');
  section.className = 'fb-process-message-form';
  section.innerHTML = `<tf-select data-message-name wrap-selected label="${escapeAttr(text('message_name'))}">${names.map((name) => `<option value="${escapeAttr(name)}">${escapeHtml(name)}</option>`).join('')}</tf-select>
    <tf-textarea data-message-key label="${escapeAttr(text('message_correlation_key'))}" autogrow rows="2"></tf-textarea>
    <tf-input data-message-ttl type="number" min="1" max="604800" step="1" value="3600" label="${escapeAttr(text('message_ttl'))}"></tf-input>
    <div data-message-payload></div><tf-alert data-message-error tone="danger" hidden></tf-alert>`;
  const payload = jsonSection(text('message_payload'), null);
  section.querySelector('[data-message-payload]').append(payload);
  const error = section.querySelector('[data-message-error]');
  const command = processCommand();
  let values;
  let previousMessage = null;
  let messageId = null;
  return openFormWindow({ title: text('send_message'), icon: 'mail', width: 700, sections: [section], submitLabel: text('send_message'),
    validate: () => {
      error.hidden = true;
      const name = section.querySelector('[data-message-name]').value;
      const key = section.querySelector('[data-message-key]').value;
      const ttl = Number(section.querySelector('[data-message-ttl]').value);
      if (!names.includes(name) || !key || new TextEncoder().encode(key).length > 256 || /[\u0000-\u001f\u007f]/u.test(key) || !Number.isInteger(ttl) || ttl < 1 || ttl > 604800) {
        error.setAttribute('message', text('message_invalid_send'));
        error.hidden = false;
        return false;
      }
      if (!validateJson(payload)) return false;
      const message = { target, messageName: name, correlationKey: key, payload: payload.jsonValue, ttlSeconds: ttl };
      const signature = JSON.stringify(message);
      if (signature !== previousMessage) {
        previousMessage = signature;
        messageId = crypto.randomUUID();
      }
      values = { messageId, ...message };
      return true;
    },
    collect: () => values,
    onSubmit: async (message) => {
      const response = await ApiBinary.one('processMessageSendRequest', command(message));
      onSent(response.message);
      return { message: text('message_sent_status', { status: text(`message_status_${response.message.status.toLowerCase()}`) }) };
    },
  });
}

export async function openProcessMessageDetail(summary, onChanged = () => {}) {
  const win = readWindow(text('message_detail'), 'mail', 760);
  const host = win.querySelector('[data-content]');
  let current = summary;
  const cancelCommand = processCommand();
  const resolveCommand = processCommand();
  let readGeneration = 0;
  async function render({ clearErrorOnSuccess = false } = {}) {
    const generation = ++readGeneration;
    let response;
    try {
      response = await ApiBinary.one('processMessageGetRequest', { senderUserId: current.senderUserId, messageId: current.messageId });
    } catch (error) {
      if (win.isConnected && generation === readGeneration) {
        host.replaceChildren();
        current = { ...current, canResolve: false, canCancel: false, payloadAvailable: false };
      }
      throw error;
    }
    if (!win.isConnected || generation !== readGeneration) return;
    current = response.message.message;
    host.innerHTML = `<div class="fb-process-work"><div><h3>${escapeHtml(current.messageName)}</h3>
      <p>${escapeHtml(text(`message_status_${current.status.toLowerCase()}`))} · ${escapeHtml(current.correlationKey)}</p>
      <p>${escapeHtml(text('message_sender'))}: ${escapeHtml(current.senderUserId)}</p>
      <p>${escapeHtml(text('message_id'))}: ${escapeHtml(current.messageId)}</p>
      ${current.lastReason ? `<p>${escapeHtml(processLifecycleReasonText(current.lastReason))}</p>` : ''}</div></div>
      <div data-message-payload></div><div class="fb-process-actions">
      ${current.canResolve ? `<tf-button variant="secondary" data-resolve>${escapeHtml(text('resolve_message'))}</tf-button>` : ''}
      ${current.canCancel ? `<tf-button variant="danger" data-cancel-message>${escapeHtml(text('cancel_message'))}</tf-button>` : ''}</div>`;
    const payloadHost = host.querySelector('[data-message-payload]');
    if (current.payloadAvailable) payloadHost.append(jsonSection(text('message_payload'), response.message.payload, false));
    else payloadHost.textContent = text('message_payload_unavailable');
    host.querySelector('[data-cancel-message]')?.addEventListener('click', async () => {
      const approved = await customElements.get('tf-window').confirm({ title: text('cancel_message'), message: text('cancel_message_hint'), danger: true,
        confirmLabel: text('cancel_message'), cancelLabel: I18n.t('common.cancel') });
      if (!approved || !win.isConnected) return;
      try {
        const result = await ApiBinary.one('processMessageCancelRequest', cancelCommand({ messageId: current.messageId, expectedRevision: current.revision }));
        current = result.message;
        win.querySelector('[data-error]').hidden = true;
        await render();
        onChanged();
      } catch (error) { showError(win, error); }
    });
    host.querySelector('[data-resolve]')?.addEventListener('click', () => {
      const section = document.createElement('section');
      section.className = 'fb-process-message-form';
      const target = current.target.Catch;
      section.innerHTML = `<tf-textarea data-resolve-instance label="${escapeAttr(text('message_target_instance'))}" value="${escapeAttr(target?.instanceId || '')}" autogrow rows="2"></tf-textarea>
        <tf-select data-resolve-subscription wrap-selected label="${escapeAttr(text('message_subscription'))}"><option value="">${escapeHtml(text('choose_subscription'))}</option></tf-select>
        <tf-button variant="secondary" data-load-subscriptions>${escapeHtml(text('load_subscriptions'))}</tf-button>
        <tf-alert data-resolve-error tone="danger" hidden></tf-alert>`;
      const errorHost = section.querySelector('[data-resolve-error]');
      const select = section.querySelector('[data-resolve-subscription]');
      const instanceControl = section.querySelector('[data-resolve-instance]');
      let loadGeneration = 0;
      instanceControl.addEventListener('input', () => {
        loadGeneration += 1;
        select.replaceChildren();
        errorHost.hidden = true;
      });
      instanceControl.addEventListener('change', () => {
        loadGeneration += 1;
        select.replaceChildren();
        errorHost.hidden = true;
      });
      section.querySelector('[data-load-subscriptions]').addEventListener('click', async () => {
        const generation = ++loadGeneration;
        select.replaceChildren();
        const empty = document.createElement('option'); empty.value = ''; empty.textContent = text('choose_subscription'); select.append(empty);
        const targetInstanceId = instanceControl.value;
        if (!targetInstanceId) return;
        try {
          let offset = 0;
          let more = true;
          while (more) {
            const result = await ApiBinary.one('processInstanceGetRequest', { instanceId: targetInstanceId,
              pages: { subscriptions: { offset, limit: 20 } } });
            if (generation !== loadGeneration || instanceControl.value !== targetInstanceId || !section.isConnected) return;
            if (result.instance.definitionId !== target.definitionId) throw new Error(text('message_target_mismatch'));
            for (const subscription of result.instance.subscriptions) {
              if (subscription.status !== 'Open' || subscription.messageName !== current.messageName || subscription.correlationKey !== current.correlationKey) continue;
              const option = document.createElement('option'); option.value = subscription.subscriptionId;
              option.textContent = `${subscription.nodeName || subscription.nodeId} · ${subscription.subscriptionId}`;
              select.append(option);
            }
            more = result.instance.pages.subscriptions.hasMore;
            offset = result.instance.pages.subscriptions.nextOffset;
          }
          errorHost.hidden = true;
        } catch (error) {
          if (generation !== loadGeneration || instanceControl.value !== targetInstanceId || !section.isConnected) return;
          errorHost.setAttribute('message', error.message);
          errorHost.hidden = false;
        }
      });
      openFormWindow({ title: text('resolve_message'), icon: 'mail', sections: [section], submitLabel: text('resolve_message'),
        canSubmit: () => !!section.querySelector('[data-resolve-instance]').value && !!select.value,
        collect: () => ({ messageId: current.messageId, expectedRevision: current.revision,
          instanceId: section.querySelector('[data-resolve-instance]').value, subscriptionId: select.value }),
        onSubmit: async (value) => {
          const result = await ApiBinary.one('processMessageResolveRequest', resolveCommand(value));
          current = result.message;
          win.querySelector('[data-error]').hidden = true;
          await render();
          onChanged();
          return { message: text('message_resolved') };
        },
      });
    });
    if (clearErrorOnSuccess) win.querySelector('[data-error]').hidden = true;
  }
  try { await render({ clearErrorOnSuccess: true }); } catch (error) { showError(win, error); }
  const timer = setInterval(() => { render().catch((error) => showError(win, error)); }, 3000);
  win.addEventListener('closed', () => { clearInterval(timer); readGeneration += 1; }, { once: true });
  return win;
}

export async function openProcessMessages(definitionId = null, instanceId = null) {
  const win = readWindow(text('messages'), 'mail');
  const host = win.querySelector('[data-content]');
  host.innerHTML = `<tf-table data-messages empty-message="${escapeAttr(text('messages_empty'))}" page-size="20" page="1">
    <tf-column key="messageName" label="${escapeAttr(text('message_name'))}"></tf-column>
    <tf-column key="statusLabel" label="${escapeAttr(text('status'))}"></tf-column>
    <tf-column key="received" label="${escapeAttr(text('updated'))}"></tf-column></tf-table>`;
  const table = host.querySelector('[data-messages]');
  table.rowKey = 'messageId';
  table.rowActionsKey = (row) => row.messageId;
  table.rowActions = (_row, _index, selected) => {
    const button = document.createElement('tf-button');
    button.setAttribute('variant', 'secondary');
    button.textContent = text('message_detail');
    button.addEventListener('click', () => openProcessMessageDetail(selected(), load));
    return button;
  };
  let offset = 0;
  let generation = 0;
  async function load() {
    const current = ++generation;
    try {
      const response = await ApiBinary.one('processMessageListRequest', { definitionId, instanceId, offset, limit: 20 });
      if (!win.isConnected || current !== generation) return;
      table.setAttribute('total', String(response.total));
      table.setAttribute('page', String(offset / 20 + 1));
      table.rows = response.messages.map((message) => ({ ...message,
        statusLabel: text(`message_status_${message.status.toLowerCase()}`), received: date(message.receivedAtMs) }));
    } catch (error) { if (current === generation) showError(win, error); }
  }
  table.addEventListener('page-change', (event) => { offset = (event.detail.page - 1) * 20; load(); });
  await load();
  return win;
}

export function openProcessCalendar(calendar, pin, pinState, readOnly, onSave) {
  const current = structuredClone(calendar ?? {
    name: '', weeklyWindows: [1, 2, 3, 4, 5].map((weekday) => ({ weekday, startMinute: 540, endMinute: 1020 })),
    manualDaysOff: [], holidayPolicy: 'PolandStatutory',
  });
  const section = document.createElement('section');
  section.className = 'fb-process-calendar';
  section.innerHTML = `<div class="fb-process-calendar-heading"><span>${escapeHtml(text('calendar_enabled'))}</span>
      <tf-toggle data-calendar-enabled aria-label="${escapeAttr(text('calendar_enabled'))}" ${calendar ? 'checked' : ''} ${readOnly ? 'disabled' : ''}></tf-toggle></div>
    <div data-calendar-fields>
      <tf-textarea data-calendar-name label="${escapeAttr(text('calendar_name'))}" value="${escapeAttr(current.name)}" maxlength="256" autogrow rows="2" ${readOnly ? 'disabled' : ''}></tf-textarea>
      <tf-select data-calendar-policy label="${escapeAttr(text('calendar_holiday_policy'))}" value="${escapeAttr(current.holidayPolicy)}" ${readOnly ? 'disabled' : ''}>
        <option value="PolandStatutory">${escapeHtml(text('calendar_policy_poland'))}</option>
        <option value="None">${escapeHtml(text('calendar_policy_none'))}</option>
      </tf-select>
      <p class="fb-field-hint">${escapeHtml(text('calendar_timezone_hint'))}</p>
      <h3>${escapeHtml(text('calendar_windows'))}</h3><div data-calendar-windows></div>
      ${readOnly ? '' : `<tf-button variant="secondary" data-calendar-add-window>${escapeHtml(text('calendar_add_window'))}</tf-button>`}
      <h3>${escapeHtml(text('calendar_days_off'))}</h3><div data-calendar-days></div>
      ${readOnly ? '' : `<tf-button variant="secondary" data-calendar-add-day>${escapeHtml(text('calendar_add_day'))}</tf-button>`}
    </div>
    <p class="fb-field-hint" data-calendar-state>${escapeHtml(text(`calendar_state_${(pinState ?? 'Unpinned').toLowerCase()}`))}</p>
    ${pin ? `<p class="fb-field-hint">${escapeHtml(text('calendar_pin_info', {
      release: pin.legalRelease.releaseId, asOf: pin.legalRelease.asOfDate,
      timezone: pin.timezoneData.releaseId, digest: pin.sha256,
    }))}</p><p class="fb-field-hint" data-calendar-coverage>${escapeHtml(text('calendar_coverage', {
      from: pin.legalRelease.validFrom, until: pin.legalRelease.validUntil,
    }))}</p><p class="fb-field-hint">${escapeHtml(text('calendar_projection_hint'))}</p>` : ''}
    <tf-alert data-calendar-error tone="danger" hidden></tf-alert>`;
  const fields = section.querySelector('[data-calendar-fields]');
  const enabled = section.querySelector('[data-calendar-enabled]');
  fields.hidden = !enabled.checked;
  enabled.addEventListener('change', (event) => { fields.hidden = !(event.detail?.checked ?? enabled.checked); });
  const windows = section.querySelector('[data-calendar-windows]');
  const days = section.querySelector('[data-calendar-days]');
  const minuteText = (minute) => `${String(Math.floor(minute / 60)).padStart(2, '0')}:${String(minute % 60).padStart(2, '0')}`;
  const addWindow = (window) => {
    const row = document.createElement('div');
    row.className = 'fb-calendar-row';
    row.innerHTML = `<tf-select data-calendar-weekday label="${escapeAttr(text('calendar_weekday'))}" value="${Number(window.weekday)}" ${readOnly ? 'disabled' : ''}>
      ${[1, 2, 3, 4, 5, 6, 7].map((day) => `<option value="${day}">${escapeHtml(text(`calendar_weekday_${day}`))}</option>`).join('')}</tf-select>
      <tf-input data-calendar-start label="${escapeAttr(text('calendar_start'))}" value="${minuteText(window.startMinute)}" placeholder="09:00" ${readOnly ? 'disabled' : ''}></tf-input>
      <tf-input data-calendar-end label="${escapeAttr(text('calendar_end'))}" value="${minuteText(window.endMinute)}" placeholder="17:00" ${readOnly ? 'disabled' : ''}></tf-input>
      ${readOnly ? '' : `<tf-button variant="ghost" data-calendar-remove aria-label="${escapeAttr(text('calendar_remove_window'))}">${escapeHtml(text('calendar_remove'))}</tf-button>`}`;
    windows.append(row);
  };
  const addDay = (day) => {
    const row = document.createElement('div');
    row.className = 'fb-calendar-row';
    row.innerHTML = `<tf-input data-calendar-date type="date" label="${escapeAttr(text('calendar_date'))}" value="${escapeAttr(day.date)}" ${readOnly ? 'disabled' : ''}></tf-input>
      <tf-textarea data-calendar-reason label="${escapeAttr(text('calendar_reason'))}" value="${escapeAttr(day.reason)}" maxlength="128" autogrow rows="2" ${readOnly ? 'disabled' : ''}></tf-textarea>
      ${readOnly ? '' : `<tf-button variant="ghost" data-calendar-remove aria-label="${escapeAttr(text('calendar_remove_day'))}">${escapeHtml(text('calendar_remove'))}</tf-button>`}`;
    days.append(row);
  };
  current.weeklyWindows.forEach(addWindow);
  current.manualDaysOff.forEach(addDay);
  section.addEventListener('click', (event) => {
    if (readOnly) return;
    if (event.target.closest('[data-calendar-add-window]')) addWindow({ weekday: 1, startMinute: 540, endMinute: 1020 });
    else if (event.target.closest('[data-calendar-add-day]')) addDay({ date: '', reason: '' });
    else event.target.closest('[data-calendar-remove]')?.closest('.fb-calendar-row')?.remove();
  });
  const error = section.querySelector('[data-calendar-error]');
  let result;
  const fail = (message) => { error.setAttribute('message', message); error.hidden = false; return false; };
  const parseMinute = (value) => {
    if (!/^\d{2}:\d{2}$/.test(value)) throw new Error(text('calendar_invalid_window'));
    const [hour, minute] = value.split(':').map(Number);
    if (hour > 24 || minute > 59 || (hour === 24 && minute !== 0)) throw new Error(text('calendar_invalid_window'));
    return hour * 60 + minute;
  };
  return openFormWindow({ title: text('calendar_title'), icon: 'clock', width: 720, sections: [section],
    submitLabel: text('apply'), canSubmit: () => !readOnly,
    validate: () => {
      error.hidden = true;
      if (!enabled.checked) { result = null; return true; }
      try {
        const weeklyWindows = Array.from(windows.children).map((row) => ({
          weekday: Number(row.querySelector('[data-calendar-weekday]').value),
          startMinute: parseMinute(row.querySelector('[data-calendar-start]').value),
          endMinute: parseMinute(row.querySelector('[data-calendar-end]').value),
        })).sort((a, b) => a.weekday - b.weekday || a.startMinute - b.startMinute || a.endMinute - b.endMinute);
        const dayCounts = new Map();
        if (!weeklyWindows.length || weeklyWindows.length > 56 || weeklyWindows.some((window, index) => {
          dayCounts.set(window.weekday, (dayCounts.get(window.weekday) ?? 0) + 1);
          return window.weekday < 1 || window.weekday > 7 || dayCounts.get(window.weekday) > 8 ||
          window.startMinute >= window.endMinute ||
          (index > 0 && weeklyWindows[index - 1].weekday === window.weekday && weeklyWindows[index - 1].endMinute > window.startMinute);
        })) {
          return fail(text('calendar_invalid_window'));
        }
        const manualDaysOff = Array.from(days.children).map((row) => ({
          date: row.querySelector('[data-calendar-date]').value,
          reason: row.querySelector('[data-calendar-reason]').value,
        })).sort((a, b) => a.date.localeCompare(b.date));
        if (manualDaysOff.length > 256 || manualDaysOff.some((day, index) =>
          !/^\d{4}-\d{2}-\d{2}$/.test(day.date) ||
          day.date < '2024-01-01' || day.date >= '2041-01-01' ||
          Number.isNaN(Date.parse(`${day.date}T00:00:00Z`)) || new Date(`${day.date}T00:00:00Z`).toISOString().slice(0, 10) !== day.date ||
          !day.reason.trim() || new TextEncoder().encode(day.reason).length > 128 || /[\x00-\x1f\x7f]/.test(day.reason) ||
          (index > 0 && manualDaysOff[index - 1].date === day.date))) {
          return fail(text('calendar_invalid_day'));
        }
        const name = section.querySelector('[data-calendar-name]').value.trim();
        if (!name || new TextEncoder().encode(name).length > 256) return fail(text('calendar_invalid_name'));
        result = { name, weeklyWindows, manualDaysOff,
          holidayPolicy: section.querySelector('[data-calendar-policy]').value };
        return true;
      } catch (failure) { return fail(failure.message); }
    }, collect: () => result,
    onSubmit: async (value) => { onSave(value); return { message: text('calendar_updated') }; },
  });
}
