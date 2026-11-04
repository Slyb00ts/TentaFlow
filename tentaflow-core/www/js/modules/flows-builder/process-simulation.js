// ============ File: process-simulation.js — deterministic private process simulation UI ============

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { processDistinctLabels, processElementNames, processExplainError, processRepeatedElements, processStartEntryLabels } from './bpmn.js';
import { answerFormHtml, fillAnswerForm, readAnswerDraft, showAnswerErrors } from './simulation-answer-form.js';
import { blankServiceDraft, readServiceAnswer, readServiceFixtures, simulationServiceSteps, simulationWaitSteps } from './simulation-service.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-select.js';
import '/js/components/tf-window.js';

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

export function simulationWindow(view, model = null) {
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
  // The run reports elements by id; the author knows them by the names of the
  // model that was started.
  const elementNames = model ? processElementNames(model) : new Map();
  const elementName = (nodeId, name) => String(name || '').trim() || elementNames.get(nodeId) || text('element_unavailable');
  // What the author typed into a result form survives the next re-render, which
  // rebuilds every control.
  const drafts = new Map();

  const setError = (error) => {
    const alert = win.querySelector('[data-error]');
    const message = error?.message || String(error);
    alert.setAttribute('message', model ? processExplainError(message, model) : message);
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
    content.querySelectorAll('[data-simulation-wait]').forEach((article) => {
      drafts.set(article.dataset.simulationWait, readAnswerDraft(article));
    });
    const waits = simulationWaitSteps(current.serviceWaits, model);
    for (const jobId of [...drafts.keys()]) {
      if (!waits.some(({ wait }) => wait.jobId === jobId)) drafts.delete(jobId);
    }
    const clock = current.clock;
    const instanceStatus = current.instance?.status || text('status_running');
    const tasks = current.userTasks || [];
    const timers = current.timers || [];
    const events = current.events || [];
    const shownEvents = events.slice(-SIMULATION_EVENTS_SHOWN);
    // The server omits what does not fit one frame and this view shows only the
    // newest rows of the rest, so the label counts both kinds of hidden events.
    const hiddenEvents = (current.eventsOmitted || 0) + events.length - shownEvents.length;
    content.innerHTML = `
      <div class="fb-simulation-toolbar">
        <div><strong>${escapeHtml(text('simulation_clock'))}</strong>: ${escapeHtml(String(clock.nowMs))} / ${escapeHtml(String(clock.horizonMs))} ms · ${escapeHtml(String(clock.stepIndex))}</div>
        <div><span>${escapeHtml(text('status'))}: ${escapeHtml(instanceStatus)}</span>
          <tf-button variant="secondary" size="sm" icon="plus" data-simulation-advance ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_advance'))}</tf-button></div>
      </div>
      ${waits.length ? `<section class="fb-simulation-section" data-simulation-waits><h3>${escapeHtml(text('simulation_waits', { count: waits.length }))}</h3>
        ${waits.map(({ wait, step }) => `<article class="fb-simulation-wait" data-simulation-wait="${escapeAttr(wait.jobId)}" data-node-id="${escapeAttr(wait.nodeId)}">
          <div><strong>${escapeHtml(elementName(wait.nodeId))}</strong>
            <tf-chip status="info">${escapeHtml(text(wait.mode === 'Fixture' ? 'simulation_wait_fixture' : 'simulation_wait_result'))}</tf-chip></div>
          <section class="fb-process-json"><h4>${escapeHtml(text('simulation_wait_input'))}</h4>
            <tf-code-editor readonly data-simulation-wait-input language="json" aria-label="${escapeAttr(text('simulation_wait_input'))}"></tf-code-editor></section>
          ${answerFormHtml(step, drafts.get(wait.jobId) ?? blankServiceDraft())}
          <tf-button variant="primary" size="sm" data-simulation-resolve="${escapeAttr(wait.jobId)}" ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_resolve'))}</tf-button>
        </article>`).join('')}
      </section>` : ''}
      <section class="fb-simulation-section"><h3>${escapeHtml(text('simulation_tasks'))}</h3>
        ${tasks.length ? tasks.map((task) => {
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
                <tf-button variant="primary" size="sm" data-simulation-task="${id}" data-simulation-manual="false" ${busy ? 'disabled' : ''}>${escapeHtml(text('simulation_complete_task'))}</tf-button>`;
          return `<article class="fb-simulation-task"><div><strong>${escapeHtml(elementName(task.nodeId, task.name))}</strong> · ${escapeHtml(task.kind || '')}</div>
            <div>${escapeHtml(task.instructions || '')}</div>
            ${controls}
          </article>`;
        }).join('') : `<p>${escapeHtml(text('simulation_empty_tasks'))}</p>`}
      </section>
      <section class="fb-simulation-section"><h3>${escapeHtml(text('simulation_timers'))}</h3>
        ${timers.length ? timers.map((timer) => {
          const status = String(timer.status || '').toLowerCase();
          const statusKey = ['pending', 'fired', 'blocked'].includes(status)
            ? `simulation_timer_${status}` : null;
          const due = timer.dueAtMs == null ? '—' : String(timer.dueAtMs);
          return `<article class="fb-simulation-timer"><div><strong>${escapeHtml(elementName(timer.nodeId, timer.nodeName))}</strong> · ${escapeHtml(String(timer.kind || ''))}
            <tf-chip status="info">${escapeHtml(statusKey ? text(statusKey) : String(timer.status || ''))}</tf-chip></div>
            <div>${escapeHtml(text('simulation_timer_due'))}: ${escapeHtml(due)} · ${escapeHtml(text('simulation_timer_occurrence'))}: ${escapeHtml(String(timer.occurrence ?? 0))}</div>
          </article>`;
        }).join('') : `<p>${escapeHtml(text('simulation_empty_timers'))}</p>`}
      </section>
      <section class="fb-simulation-section"><h3>${escapeHtml(text('simulation_events'))}</h3>
        ${events.length ? `<ol class="fb-simulation-events">${shownEvents.map((event) => `<li><span>${escapeHtml(String(event.atMs))}</span> ${escapeHtml(event.kind)}${event.nodeId ? ` · ${escapeHtml(elementName(event.nodeId))}` : ''}</li>`).join('')}</ol>` : `<p>${escapeHtml(text('simulation_empty_events'))}</p>`}
        ${hiddenEvents > 0 ? `<p data-simulation-events-omitted>${escapeHtml(text('simulation_events_omitted', { count: hiddenEvents }))}</p>` : ''}
        ${current.traceStepsOmitted > 0 ? `<p data-simulation-trace-omitted>${escapeHtml(text('simulation_trace_steps_omitted', { count: current.traceStepsOmitted }))}</p>` : ''}
      </section>`;
    content.querySelectorAll('tf-code-editor[data-simulation-output]').forEach((editor) => {
      editor.value = '{}';
    });
    content.querySelectorAll('[data-simulation-wait]').forEach((article) => {
      const wait = waits.find((entry) => entry.wait.jobId === article.dataset.simulationWait).wait;
      article.querySelector('[data-simulation-wait-input]').value = JSON.stringify(wait.input ?? null, null, 2);
      fillAnswerForm(article, drafts.get(wait.jobId) ?? blankServiceDraft());
    });
  };

  content.addEventListener('click', (event) => {
    const advance = event.target.closest('[data-simulation-advance]');
    if (advance) {
      run(async () => (await ApiBinary.one('processSimulationAdvanceRequest', { simulationId: current.simulationId })).view);
      return;
    }
    const resolve = event.target.closest('[data-simulation-resolve]');
    if (resolve) {
      const article = resolve.closest('[data-simulation-wait]');
      const wait = (current.serviceWaits || []).find((entry) => entry.jobId === resolve.dataset.simulationResolve);
      if (!wait || busy) return;
      const [{ step }] = simulationWaitSteps([wait], model);
      const answer = readServiceAnswer(step, readAnswerDraft(article));
      showAnswerErrors(article, answer.errors);
      if (answer.errors) return;
      const simulationId = current.simulationId;
      run(async () => (await ApiBinary.one('processSimulationServiceResolveRequest', {
        simulationId, nodeId: wait.nodeId, jobId: wait.jobId, result: answer.result, verdict: answer.verdict,
      })).view);
      return;
    }
    const taskButton = event.target.closest('[data-simulation-task]');
    if (!taskButton) return;
    const simulationId = current.simulationId;
    const userTaskId = taskButton.dataset.simulationTask;
    if (taskButton.dataset.simulationDecide) {
      const approved = taskButton.dataset.simulationDecide === 'true';
      run(async () => (await ApiBinary.one('processSimulationUserTaskCompleteRequest', { simulationId, userTaskId, outputs: {}, approved })).view);
      return;
    }
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

// One row per Service step the simulation can run: the operator answers when
// the step is reached, or a prepared result applies by itself. Running the
// step for real is listed but cannot be chosen, because the engine refuses it.
function fixturesSection(model) {
  const steps = simulationServiceSteps(model);
  if (!steps.length) return null;
  const names = processElementNames(model);
  const labels = processDistinctLabels(steps.map((step) => [step.nodeId, names.get(step.nodeId)]));
  const section = document.createElement('section');
  section.className = 'fb-simulation-fixtures';
  section.innerHTML = `<h3>${escapeHtml(text('simulation_fixtures'))}</h3><p>${escapeHtml(text('simulation_fixtures_hint'))}</p>
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
    submitLabel: text('simulation_start'), anchor,
    errorMessage: (error) => processExplainError(error?.message, definition.model),
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
      simulationWindow(response.view, definition.model);
      return { message: text('simulation_started') };
    },
  });
}
