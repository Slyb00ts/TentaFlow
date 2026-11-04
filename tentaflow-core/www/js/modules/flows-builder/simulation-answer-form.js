// ============ File: simulation-answer-form.js — the result form of a simulated Service step ============

import { escapeAttr, escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { SERVICE_VERDICTS, simulationServiceOutcomes } from './simulation-service.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';

const text = (key, values) => I18n.t(`bpmn.${key}`, values);

const OUTCOME_KEYS = { Completed: 'completed', Error: 'error', NeedsHuman: 'needs_human', Cancelled: 'cancelled' };

const field = (container, name) => container.querySelector(`[data-answer-field="${name}"]`);

/**
 * The fields of one answer: outcome, error code, summary, outputs and, for a
 * step checked by a test or a critic, the verdict. The editor value is set by
 * `fillAnswerForm` once the markup is in the document.
 */
export function answerFormHtml(step, draft) {
  const outcomes = simulationServiceOutcomes(step);
  const verdict = step.machineVerdict ? `<div data-answer-verdict>
    <tf-select data-answer-field="verdict" wrap-selected label="${escapeAttr(text('simulation_verdict'))}" hint="${escapeAttr(text('simulation_verdict_hint'))}" value="${escapeAttr(draft.verdict)}">
      <option value="">${escapeHtml(text('simulation_verdict_choose'))}</option>
      ${SERVICE_VERDICTS.map((value) => `<option value="${value}">${escapeHtml(text(`simulation_verdict_${value.toLowerCase()}`))}</option>`).join('')}
    </tf-select>
    <tf-alert data-answer-error="verdict" tone="danger" hidden></tf-alert></div>` : '';
  return `<div class="fb-simulation-answer" data-has-expression="${step.hasResultExpression ? 'true' : 'false'}">
    <tf-select data-answer-field="outcome" wrap-selected label="${escapeAttr(text('simulation_outcome'))}"${step.hasResultExpression ? ` hint="${escapeAttr(text('simulation_outcome_expression_hint'))}"` : ''} value="${escapeAttr(draft.outcome)}">
      ${outcomes.map((value) => `<option value="${value}">${escapeHtml(text(`simulation_outcome_${OUTCOME_KEYS[value]}`))}</option>`).join('')}
    </tf-select>
    <tf-alert data-answer-error="outcome" tone="danger" hidden></tf-alert>
    <tf-input data-answer-field="code" label="${escapeAttr(text('simulation_result_code'))}" hint="${escapeAttr(text('simulation_result_code_hint'))}" value="${escapeAttr(draft.code)}"></tf-input>
    <tf-alert data-answer-error="code" tone="danger" hidden></tf-alert>
    <tf-input data-answer-field="summary" label="${escapeAttr(text('simulation_result_summary'))}" value="${escapeAttr(draft.summary)}"></tf-input>
    <tf-alert data-answer-error="summary" tone="danger" hidden></tf-alert>
    <section class="fb-process-json"><h4>${escapeHtml(text('simulation_result_outputs'))}</h4>
      <tf-code-editor data-answer-field="outputs" language="json" aria-label="${escapeAttr(text('simulation_result_outputs'))}"></tf-code-editor>
      <p class="fb-field-hint">${escapeHtml(text('simulation_editor_escape_hint'))}</p>
      <tf-alert data-answer-error="outputs" tone="danger" hidden></tf-alert>
    </section>
    ${verdict}
  </div>`;
}

// A step without a result expression only ever reaches its check with a result that
// completes, so the verdict is asked for only then; with an expression the engine decides.
export function syncAnswerForm(container) {
  const form = container.querySelector('.fb-simulation-answer');
  const verdict = container.querySelector('[data-answer-verdict]');
  if (!form || !verdict) return;
  const select = field(container, 'outcome');
  const outcome = select?.value || select?.getAttribute('value');
  verdict.hidden = form.dataset.hasExpression !== 'true' && outcome !== 'Completed';
}

export function fillAnswerForm(container, draft) {
  const outputs = field(container, 'outputs');
  if (outputs) outputs.value = draft.outputs;
  syncAnswerForm(container);
}

export function readAnswerDraft(container) {
  return {
    outcome: field(container, 'outcome')?.value ?? 'Completed',
    code: field(container, 'code')?.value ?? '',
    summary: field(container, 'summary')?.value ?? '',
    outputs: field(container, 'outputs')?.value ?? '',
    verdict: field(container, 'verdict')?.value ?? '',
  };
}

// `errors` maps a field to the i18n key of what is wrong with it; fields not in
// it are cleared.
export function showAnswerErrors(container, errors = {}) {
  let first = null;
  container.querySelectorAll('[data-answer-error]').forEach((alert) => {
    const error = errors[alert.dataset.answerError];
    if (error) {
      const { key, values } = typeof error === 'string' ? { key: error } : error;
      alert.setAttribute('message', text(key, values));
      first ??= alert;
    }
    alert.hidden = !error;
  });
  // The problem may sit below the fold of a tall window.
  first?.scrollIntoView?.({ block: 'center' });
}

export function errorMessage(error) {
  const { key, values } = typeof error === 'string' ? { key: error } : error;
  return text(key, values);
}
