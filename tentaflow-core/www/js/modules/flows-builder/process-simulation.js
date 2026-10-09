// ============ File: process-simulation.js — deterministic private process simulation UI ============

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-select.js';
import '/js/components/tf-window.js';

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

function simulationWindow(view) {
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
  let current = view;
  let busy = false;

  const setError = (error) => {
    const alert = win.querySelector('[data-error]');
    alert.setAttribute('message', error?.message || String(error));
    alert.hidden = false;
  };

  const run = async (action) => {
    if (busy || !win.isConnected) return;
    busy = true;
    win.querySelectorAll('tf-button').forEach((button) => button.setAttribute('disabled', ''));
    win.querySelector('[data-error]').hidden = true;
    try {
      current = await action();
      render();
    } catch (error) {
      setError(error);
    } finally {
      busy = false;
      if (win.isConnected) render();
    }
  };

  const render = () => {
    if (!win.isConnected) return;
    const clock = current.clock;
    const instanceStatus = current.instance?.status || text('status_running');
    const tasks = current.userTasks || [];
    const timers = current.timers || [];
    const events = current.events || [];
    content.innerHTML = `
      <div class="fb-simulation-toolbar">
        <div><strong>${escapeHtml(text('simulation_clock'))}</strong>: ${escapeHtml(String(clock.nowMs))} / ${escapeHtml(String(clock.horizonMs))} ms · ${escapeHtml(String(clock.stepIndex))}</div>
        <div><span>${escapeHtml(text('status'))}: ${escapeHtml(instanceStatus)}</span>
          <tf-button variant="secondary" size="sm" icon="plus" data-simulation-advance ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_advance'))}</tf-button></div>
      </div>
      <section class="fb-simulation-section"><h3>${escapeHtml(text('simulation_tasks'))}</h3>
        ${tasks.length ? tasks.map((task) => {
          const manual = task.kind === 'Manual';
          return `<article class="fb-simulation-task"><div><strong>${escapeHtml(task.name || task.nodeId || task.userTaskId)}</strong> · ${escapeHtml(task.kind || '')}</div>
            <div>${escapeHtml(task.instructions || '')}</div>
            ${manual ? '' : `<tf-code-editor data-simulation-output="${escapeAttr(task.userTaskId)}" language="json" aria-label="${escapeAttr(text('simulation_outputs'))}"></tf-code-editor>`}
            <tf-button variant="${manual ? 'secondary' : 'primary'}" size="sm" data-simulation-task="${escapeAttr(task.userTaskId)}" data-simulation-manual="${manual ? 'true' : 'false'}" ${busy ? 'disabled' : ''}>${escapeHtml(text(manual ? 'simulation_acknowledge_task' : 'simulation_complete_task'))}</tf-button>
          </article>`;
        }).join('') : `<p>${escapeHtml(text('simulation_empty_tasks'))}</p>`}
      </section>
      <section class="fb-simulation-section"><h3>${escapeHtml(text('simulation_timers'))}</h3>
        ${timers.length ? timers.map((timer) => {
          const status = String(timer.status || '').toLowerCase();
          const statusKey = ['pending', 'fired', 'blocked'].includes(status)
            ? `simulation_timer_${status}` : null;
          const due = timer.dueAtMs == null ? '—' : String(timer.dueAtMs);
          return `<article class="fb-simulation-timer"><div><strong>${escapeHtml(timer.nodeName || timer.nodeId || timer.timerId)}</strong> · ${escapeHtml(String(timer.kind || ''))}
            <tf-chip status="info">${escapeHtml(statusKey ? text(statusKey) : String(timer.status || ''))}</tf-chip></div>
            <div>${escapeHtml(text('simulation_timer_due'))}: ${escapeHtml(due)} · ${escapeHtml(text('simulation_timer_occurrence'))}: ${escapeHtml(String(timer.occurrence ?? 0))}</div>
          </article>`;
        }).join('') : `<p>${escapeHtml(text('simulation_empty_timers'))}</p>`}
      </section>
      <section class="fb-simulation-section"><h3>${escapeHtml(text('simulation_events'))}</h3>
        ${events.length ? `<ol class="fb-simulation-events">${events.slice(-50).map((event) => `<li><span>${escapeHtml(String(event.atMs))}</span> ${escapeHtml(event.kind)}${event.nodeId ? ` · ${escapeHtml(event.nodeId)}` : ''}</li>`).join('')}</ol>` : `<p>${escapeHtml(text('simulation_empty_events'))}</p>`}
        ${current.eventsOmitted > 0 ? `<p>${escapeHtml(text('simulation_events_omitted', { count: current.eventsOmitted }))}</p>` : ''}
      </section>`;
    content.querySelectorAll('tf-code-editor[data-simulation-output]').forEach((editor) => {
      editor.value = '{}';
    });
  };

  content.addEventListener('click', (event) => {
    const advance = event.target.closest('[data-simulation-advance]');
    if (advance) {
      run(async () => (await ApiBinary.one('processSimulationAdvanceRequest', { simulationId: current.simulationId })).view);
      return;
    }
    const taskButton = event.target.closest('[data-simulation-task]');
    if (!taskButton) return;
    const simulationId = current.simulationId;
    const userTaskId = taskButton.dataset.simulationTask;
    if (taskButton.dataset.simulationManual === 'true') {
      run(async () => (await ApiBinary.one('processSimulationManualTaskAcknowledgeRequest', { simulationId, userTaskId })).view);
      return;
    }
    const editor = content.querySelector(`[data-simulation-output="${CSS.escape(userTaskId)}"]`);
    let outputs;
    try {
      outputs = JSON.parse(editor.value);
    } catch (error) {
      setError(new Error(text('json_invalid')));
      return;
    }
    run(async () => (await ApiBinary.one('processSimulationUserTaskCompleteRequest', { simulationId, userTaskId, outputs })).view);
  });
  win.addEventListener('closed', () => {
    const simulationId = current?.simulationId;
    if (simulationId) {
      void ApiBinary.one('processSimulationReleaseRequest', { simulationId }).catch((error) => {
        console.warn('[simulation] release request failed:', error);
      });
    }
    content.replaceChildren();
  }, { once: true });
  render();
  return win;
}

export function openProcessSimulation(definition, version, startCatalog, selectedStart, anchor = null) {
  const manualStarts = startCatalog.filter((entry) => entry.trigger?.Start?.canStart);
  const selection = document.createElement('section');
  selection.className = 'fb-process-simulation-start';
  selection.innerHTML = `<tf-select data-simulation-start label="${escapeAttr(text('start_entry'))}"><option value="">${escapeHtml(text('start_entry'))}</option>${manualStarts.map((entry, index) => `<option value="${index}">${escapeHtml(entry.processName || entry.processId)} · ${escapeHtml(entry.startNodeName || entry.startNodeId)}</option>`).join('')}</tf-select>`;
  const variables = jsonSection(text('simulation_variables'), definition.model.variables || {});
  const selectedIndex = manualStarts.findIndex((entry) => entry.processId === selectedStart?.processId
    && entry.startNodeId === selectedStart?.startNodeId);
  selection.querySelector('[data-simulation-start]').value = selectedIndex < 0 ? '' : String(selectedIndex);
  return openFormWindow({
    title: text('simulation'), icon: 'play', subject: definition.name,
    note: { text: text('simulation_hint') }, sections: [selection, variables],
    submitLabel: text('simulation_start'), anchor,
    validate: () => {
      const selected = selection.querySelector('[data-simulation-start]').value;
      if (selected !== '') {
        try {
          variables.readValue();
          return true;
        } catch (_) {
          return false;
        }
      }
      selection.querySelector('[data-simulation-start]').setAttribute('error', text('start_entry_changed'));
      return false;
    },
    canSubmit: () => selection.querySelector('[data-simulation-start]').value !== '' && !definition.archived,
    collect: () => {
      const entry = manualStarts[Number(selection.querySelector('[data-simulation-start]').value)];
      if (!entry) throw new Error(text('start_entry_changed'));
      return { entry, variables: variables.readValue() };
    },
    onSubmit: async ({ entry, variables: initialVariables }) => {
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
      });
      simulationWindow(response.view);
      return { message: text('simulation_started') };
    },
  });
}
