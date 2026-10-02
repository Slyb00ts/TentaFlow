// ============ File: flows-builder/process-monitor.js — authorized process runs, human work and durable history ============

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { checkProcessValue, processCommand, processEditorLabels, processHasTimerStart, processJson, processStatusLabel } from './bpmn.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-select.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';

const text = (key, values) => I18n.t(`bpmn.${key}`, values);
const date = (ms) => new Date(ms).toLocaleString(I18n.getLanguage());
const tone = (status) => status === 'Completed' ? 'ok' : status === 'Incident' ? 'err' : status === 'Cancelled' ? 'neutral' : 'info';

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
  if (event.kind === 'timer_armed') return text('event_timer_armed', { node, due: date(event.data.due_at_ms), timezone: event.data.timezone });
  if (event.kind === 'timer_fired') {
    if (event.data.kind === 'Boundary') return text('event_boundary_fired', {
      node, due: date(event.data.planned_due_at_ms), actual: date(event.data.fired_at_ms),
      mode: text(event.data.cancel_activity ? 'boundary_interrupting' : 'boundary_noninterrupting'),
    });
    return text('event_timer_fired', { node, due: date(event.data.planned_due_at_ms), actual: date(event.data.fired_at_ms), count: event.data.skipped_count });
  }
  if (event.kind === 'timer_cancelled') {
    if (event.data.reason === 'instance_cancelled') return text('event_timer_cancelled', { node });
    return text('event_timer_cancelled_reason', { node, reason: processTimerReasonText(event.data.reason) });
  }
  if (event.kind === 'timer_blocked') return text('event_timer_blocked', { node, message: event.data.reason, retry: date(event.data.next_check_at_ms) });
  if (event.kind === 'timer_error') return text('event_timer_error', { node, message: event.data.reason });
  if (event.kind === 'service_result') return text('event_service_result', { node, summary: event.data.summary });
  if (['incident', 'job_denied', 'job_failed'].includes(event.kind)) return text(`event_${event.kind}`, { node, message: processIncidentText(event.data) });
  return text(`event_${event.kind}`, { node });
}

export function processTimerText(timer) {
  const due = timer.dueAtMs == null ? text('timer_no_due') : new Date(timer.dueAtMs).toLocaleString(I18n.getLanguage(), { timeZone: timer.timezone, timeZoneName: 'short' });
  return text('timer_brief', { status: text(`timer_status_${timer.status.toLowerCase()}`), due, timezone: timer.timezone });
}

export function processTimerReasonText(reason) {
  switch (reason) {
    case 'instance_cancelled': return text('event_cancelled');
    case 'activity_completed': return text('timer_reason_activity_completed');
    case 'sibling_interrupted': return text('timer_reason_sibling_interrupted');
    case 'definition_archived': return text('timer_reason_definition_archived');
    case 'missed_during_archive': return text('timer_reason_missed_during_archive');
    case 'finite_schedule_exhausted_during_archive': return text('timer_reason_finite_schedule_exhausted_during_archive');
    case 'superseded_by_publication': return text('timer_reason_superseded_by_publication');
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
      ${timer.lastReason ? `<dt>${escapeHtml(text('timer_reason'))}</dt><dd>${escapeHtml(processTimerReasonText(timer.lastReason))}</dd>` : ''}</dl></div>`;
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
  const codes = ['EXPRESSION_ERROR', 'AMBIGUOUS_GATEWAY', 'NO_MATCHING_FLOW', 'HUMAN_REJECTED', 'SERVICE_ERROR', 'VERIFICATION_FAILED', 'WORKER_ERROR', 'FLOW_ERROR', 'INVALID_SERVICE_JOB', 'SOURCE_ACCESS_REVOKED', 'INTERRUPTED', 'LEASE_LOST', 'SERVICE_TIMEOUT', 'OUTPUT_LIMIT', 'TRANSITION_ERROR', 'RESULT_REJECTED', 'REVISION_CONFLICT'];
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
  host.innerHTML = `<div data-summary></div><section data-work></section><section data-incidents></section><section data-timer-section hidden><h3>${escapeHtml(text('timers'))}</h3><div data-timers></div></section>
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

  function syncWorkWindows() {
    for (const workWindow of workWindows) workWindow.dispatchEvent(new CustomEvent('change', { bubbles: true }));
  }

  async function acceptSnapshot(snapshot) {
    if (!win.isConnected) return;
    if (instance && snapshot.revision < instance.revision) {
      const response = await ApiBinary.one('processInstanceGetRequest', { instanceId });
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
    } catch (error) { showError(win, error); mutation = false; await refresh(); }
    finally { mutation = false; host.removeAttribute('inert'); }
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
      canSubmit: () => available && win.isConnected && instance.userTasks.some((current) => current.userTaskId === task.userTaskId && current.canComplete && current.status === 'Open')
        && (task.kind !== 'Verification' || approval.querySelector('tf-select').value !== ''),
      collect: () => ({ instanceId, userTaskId: task.userTaskId, expectedRevision: revision,
        outputs: task.kind === 'Verification' ? {} : outputs.jsonValue, approved: task.kind === 'Verification' ? approval.querySelector('tf-select').value === 'true' : null }),
      onSubmit: async (payload) => {
        try {
          const response = await ApiBinary.one('processUserTaskCompleteRequest', command(payload));
          if (win.isConnected) { await acceptSnapshot(response.instance); if (historyEnded) await loadHistory(); }
          return { message: text('work_completed') };
        } catch (error) {
          if (error.code === 'BadRequest') { await refresh(); revision = instance.revision; }
          throw error;
        }
      },
    });
    workWindows.add(workWindow);
    workWindow.addEventListener('closed', () => workWindows.delete(workWindow), { once: true });
  }

  function render() {
    if (!win.isConnected) return;
    renderedSnapshot = JSON.stringify(instance);
    host.querySelector('[data-summary]').innerHTML = `<div class="fb-process-summary"><h2>${escapeHtml(instance.definitionName)}</h2>
      <tf-chip status="${tone(instance.status)}">${escapeHtml(processStatusLabel(instance.status))}</tf-chip>
      <span>${escapeHtml(text('version_number', { version: instance.version }))}</span>
      ${instance.canCancel ? `<tf-button variant="danger" icon="x" data-cancel>${escapeHtml(text('cancel_instance'))}</tf-button>` : ''}</div>`;
    host.querySelector('[data-cancel]')?.addEventListener('click', async () => {
      const approved = await customElements.get('tf-window').confirm({ title: text('cancel_instance'), message: text('cancel_hint'), danger: true, confirmLabel: text('cancel_instance') });
      if (approved && win.isConnected) await applyMutation('processInstanceCancelRequest', { instanceId, expectedRevision: instance.revision }, cancelCommand);
    });
    const work = host.querySelector('[data-work]');
    work.innerHTML = `<h3>${escapeHtml(text('human_work'))}</h3>${instance.userTasks.length ? '' : `<p>${escapeHtml(text('human_work_empty'))}</p>`}`;
    for (const task of instance.userTasks) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.innerHTML = `<div><strong>${escapeHtml(task.name)}</strong><p>${escapeHtml(text(`work_kind_${task.kind.toLowerCase()}`))} · ${escapeHtml(processStatusLabel(task.status))}</p></div>
        ${task.canComplete && task.status === 'Open' ? `<tf-button variant="primary" data-complete>${escapeHtml(text(task.kind === 'Verification' ? 'review_result' : 'complete_work'))}</tf-button>` : ''}`;
      row.querySelector('[data-complete]')?.addEventListener('click', () => complete(task));
      work.appendChild(row);
    }
    const incidents = host.querySelector('[data-incidents]');
    incidents.innerHTML = instance.incidents.length ? `<h3>${escapeHtml(text('incidents'))}</h3>` : '';
    for (const incident of instance.incidents) {
      const row = document.createElement('div');
      row.className = 'fb-process-work';
      row.innerHTML = `<div><strong>${escapeHtml(incident.nodeName || text('element_unavailable'))}</strong><p>${escapeHtml(processIncidentText(incident))}</p></div>
        ${incident.canRetry && incident.jobId ? `<tf-button variant="secondary" icon="refresh" data-retry>${escapeHtml(text('retry'))}</tf-button>` : ''}`;
      row.querySelector('[data-retry]')?.addEventListener('click', async () => {
        const approved = await customElements.get('tf-window').confirm({ title: text('retry'), message: text('retry_hint'), confirmLabel: text('retry') });
        if (approved && win.isConnected) await applyMutation('processJobRetryRequest', { instanceId, jobId: incident.jobId, expectedRevision: instance.revision }, retryCommand);
      });
      incidents.appendChild(row);
    }
    host.querySelector('[data-variables]').replaceChildren(jsonSection(text('current_variables'), instance.variables, false));
    host.querySelector('[data-timer-section]').hidden = !instance.timers?.length;
    renderTimers(host.querySelector('[data-timers]'), instance.timers || []);
  }

  async function refresh() {
    if (refreshing || mutation || !win.isConnected) return;
    refreshing = true;
    const generation = ++readGeneration;
    try {
      const response = await ApiBinary.one('processInstanceGetRequest', { instanceId });
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
    note: { text: text(processHasTimerStart(definition.model) ? 'timer_start_manual_hint' : 'run_hint') }, sections: [selection, variables], submitLabel: text('run'),
    validate: () => validateJson(variables, true),
    canSubmit: () => versions.length > 0 && !definition.archived && !processHasTimerStart(definition.model),
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
