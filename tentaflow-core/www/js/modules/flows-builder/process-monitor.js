// ============ File: flows-builder/process-monitor.js — authorized process runs, human work and durable history ============

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { checkProcessValue, processCommand, processEditorLabels, processHasMessageStart, processHasTimerStart, processJson, processStatusLabel } from './bpmn.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-select.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-toggle.js';

const text = (key, values) => I18n.t(`bpmn.${key}`, values);
const date = (ms, timeZone) => new Date(ms).toLocaleString(I18n.getLanguage(), timeZone ? { timeZone } : undefined);
const tone = (status) => status === 'Completed' ? 'ok' : status === 'Incident' ? 'err' : status === 'Cancelled' ? 'neutral' : 'info';

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
    case 'scope_limit': return text('reason_scope_limit');
    case 'definition_archived': return text('timer_reason_definition_archived');
    case 'missed_during_archive': return text('timer_reason_missed_during_archive');
    case 'finite_schedule_exhausted_during_archive': return text('timer_reason_finite_schedule_exhausted_during_archive');
    case 'superseded_by_publication': return text('timer_reason_superseded_by_publication');
    case 'sender_cancelled': return text('reason_sender_cancelled');
    case 'ttl_expired': return text('reason_ttl_expired');
    case 'activation_closed': return text('reason_activation_closed');
    case 'target_instance_closed': return text('reason_target_instance_closed');
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
      if (response.timerStart) renderTimers(timers, [response.timerStart]);
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
  const codes = ['EXPRESSION_ERROR', 'AMBIGUOUS_GATEWAY', 'NO_MATCHING_FLOW', 'HUMAN_REJECTED', 'SERVICE_ERROR', 'VERIFICATION_FAILED', 'WORKER_ERROR', 'FLOW_ERROR', 'INVALID_SERVICE_JOB', 'SOURCE_ACCESS_REVOKED', 'INTERRUPTED', 'LEASE_LOST', 'SERVICE_TIMEOUT', 'OUTPUT_LIMIT', 'TRANSITION_ERROR', 'RESULT_REJECTED', 'REVISION_CONFLICT', 'SCOPE_LIMIT'];
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
    <section data-scopes><h3>${escapeHtml(text('scopes'))}</h3><div data-scope-rows></div><div data-scope-detail></div></section>
    <section data-subscriptions><h3>${escapeHtml(text('message_subscriptions'))}</h3><div data-subscription-rows></div></section>
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
  const collectionNames = ['userTasks', 'incidents', 'timers', 'subscriptions', 'eventRaces', 'outgoingMessages', 'scopes'];
  const pageSpecs = Object.fromEntries(collectionNames.map((name) => [name, { offset: 0, limit: 20 }]));
  let selectedUserTaskId = null;
  let selectedIncidentId = null;
  let selectedScopeId = null;
  let scopeReadGeneration = 0;
  const requestPages = () => ({ ...pageSpecs, selectedUserTaskId, selectedIncidentId });
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
        if (event.kind === 'service_result' && event.data.outputs !== undefined) item.append(jsonSection(text('actual_outputs'), event.data.outputs, false));
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
    selectedUserTaskId = task.userTaskId;
    let revision = instance.revision;
    const outputs = jsonSection(text(task.kind === 'Verification' ? 'actual_outputs' : 'outputs'), task.outputs, task.kind !== 'Verification');
    const approval = document.createElement('div');
    if (task.kind === 'Verification') approval.innerHTML = `<tf-select label="${escapeAttr(text('review_result'))}" data-approved>
      <option value="">${escapeHtml(text('choose_review'))}</option><option value="true">${escapeHtml(text('approve_result'))}</option><option value="false">${escapeHtml(text('reject_result'))}</option></tf-select>`;
    const command = processCommand();
    const workWindow = openFormWindow({ title: text(task.kind === 'Verification' ? 'verification_human' : 'complete_work'), icon: 'check', subject: task.name,
      note: { text: text(task.kind === 'Verification' ? 'review_hint' : 'work_hint') },
      sections: [outputs, approval], submitLabel: text('complete_work'),
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
        ${task.canComplete && task.status === 'Open' ? `<tf-button variant="primary" data-complete>${escapeHtml(text(task.kind === 'Verification' ? 'review_result' : 'complete_work'))}</tf-button>` : ''}`;
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
      row.innerHTML = `<div><strong>${escapeHtml(subscription.nodeName || subscription.nodeId)}</strong><p>${escapeHtml(subscription.messageName || subscription.errorCode || text('element_unavailable'))} · ${escapeHtml(text(`subscription_status_${subscription.status.toLowerCase()}`))}</p>
        <p>${escapeHtml(text('message_correlation_key'))}: ${escapeHtml(subscription.correlationKey || '')}</p><p>${escapeHtml(subscription.subscriptionId)}</p></div>`;
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

export function openProcessRun(definition, versions) {
  const selection = document.createElement('div');
  selection.innerHTML = `<tf-select data-version label="${escapeAttr(text('published_version'))}">${versions.map((version) => `<option value="${version.version}">${escapeHtml(text('version_number', { version: version.version }))}</option>`).join('')}</tf-select>`;
  const variables = jsonSection(text('initial_variables'), definition.model.variables);
  const command = processCommand();
  return openFormWindow({ title: text('run'), icon: 'play', subject: definition.name,
    note: { text: text(processHasTimerStart(definition.model) ? 'timer_start_manual_hint' : processHasMessageStart(definition.model) ? 'message_start_manual_hint' : 'run_hint') }, sections: [selection, variables], submitLabel: text('run'),
    validate: () => validateJson(variables, true),
    canSubmit: () => versions.length > 0 && !definition.archived && !processHasTimerStart(definition.model) && !processHasMessageStart(definition.model),
    collect: () => ({ definitionId: definition.definitionId, version: Number(selection.querySelector('tf-select').value), variables: variables.jsonValue }),
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
    <tf-alert data-declaration-error tone="danger" hidden></tf-alert>`;
  const addRow = (kind, declaration) => {
    const row = document.createElement('div');
    row.className = 'fb-declaration-row';
    row.dataset.declarationKind = kind;
    const message = kind === 'message';
    row.innerHTML = `<tf-textarea data-declaration-id label="${escapeAttr(text(message ? 'message_declaration_id' : 'error_declaration_id'))}" value="${escapeAttr(message ? declaration.messageId : declaration.errorId)}" autogrow rows="2" ${readOnly ? 'disabled' : ''}></tf-textarea>
      <tf-textarea data-declaration-name label="${escapeAttr(text('declaration_name'))}" value="${escapeAttr(declaration.name)}" autogrow rows="2" ${readOnly ? 'disabled' : ''}></tf-textarea>
      ${message ? '' : `<tf-textarea data-declaration-code label="${escapeAttr(text('error_code'))}" value="${escapeAttr(declaration.errorCode)}" autogrow rows="2" ${readOnly ? 'disabled' : ''}></tf-textarea>`}
      ${readOnly ? '' : `<tf-button variant="ghost" wrap data-remove-declaration>${escapeHtml(text('remove_declaration'))}</tf-button>`}`;
    section.querySelector(message ? '[data-message-rows]' : '[data-error-rows]').append(row);
  };
  (model.messages || []).forEach((item) => addRow('message', item));
  (model.errors || []).forEach((item) => addRow('error', item));
  section.addEventListener('click', (event) => {
    if (readOnly) return;
    if (event.target.closest('[data-add-message]')) addRow('message', { messageId: `Message_${crypto.randomUUID().replaceAll('-', '_')}`, name: '' });
    else if (event.target.closest('[data-add-error]')) addRow('error', { errorId: `Error_${crypto.randomUUID().replaceAll('-', '_')}`, name: '', errorCode: '' });
    else event.target.closest('[data-remove-declaration]')?.closest('.fb-declaration-row')?.remove();
  });
  const collect = () => {
    const rows = (kind) => Array.from(section.querySelectorAll(`[data-declaration-kind="${kind}"]`)).map((row) => {
      const id = row.querySelector('[data-declaration-id]').value;
      const name = row.querySelector('[data-declaration-name]').value;
      return kind === 'message' ? { messageId: id, name } : { errorId: id, name, errorCode: row.querySelector('[data-declaration-code]').value };
    });
    return { messages: rows('message'), errors: rows('error'), targetNamespace: section.querySelector('[data-declaration-namespace]').value || null };
  };
  return openFormWindow({ title: text('declarations'), icon: 'mail', width: 720, sections: [section],
    submitLabel: text('apply'), canSubmit: () => !readOnly, collect,
    onSubmit: async (declarations) => { onSave(declarations); return { message: text('declarations_updated') }; },
  });
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
