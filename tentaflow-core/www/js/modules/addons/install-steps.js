// =============================================================================
// File: modules/addons/install-steps.js
// Description: The app steps of the install wizard. A native package declares
//              `[[install_step]]` blocks; once its instance exists this window
//              renders each one (title, form fields, result) and runs it through
//              `addonInstanceInstallStepRequest`.
//
//              Semantics (mirrors addon/install_steps.rs): the instance is
//              already installed when this opens and stays installed whatever a
//              step reports. A failed step is shown as failed with its message
//              and can be run again; closing with a step that is not green says
//              so instead of looking finished. Nothing is remembered between
//              runs — a result describes the run that produced it.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

const TONE_OF_STATUS = { ok: 'success', warning: 'warning', failed: 'danger' };

function fieldHtml(field) {
  const id = escapeAttr(field.id);
  const label = escapeAttr(I18n.t(field.labelKey));
  const mark = field.required ? ' *' : '';
  const kind = field.kind;
  if (kind === 'checkbox') {
    const checked = field.defaultValue === 'true' ? ' checked' : '';
    return `<tf-checkbox data-field="${id}" data-kind="checkbox" label="${label}${mark}"${checked}></tf-checkbox>`;
  }
  if (kind === 'select') {
    const options = (field.options || []).map((o) => {
      const selected = o.value === field.defaultValue ? ' selected' : '';
      return `<option value="${escapeAttr(o.value)}"${selected}>${escapeHtml(I18n.t(o.labelKey))}</option>`;
    }).join('');
    const blank = field.required ? '' : '<option value=""></option>';
    return `<tf-select data-field="${id}" data-kind="select" label="${label}${mark}" value="${escapeAttr(field.defaultValue || '')}">${blank}${options}</tf-select>`;
  }
  if (kind === 'multiselect') {
    return `<tf-multiselect data-field="${id}" data-kind="multiselect" label="${label}${mark}"></tf-multiselect>`;
  }
  return `<tf-input data-field="${id}" data-kind="text" label="${label}${mark}" value="${escapeAttr(field.defaultValue || '')}"></tf-input>`;
}

function stepHtml(step, index) {
  const description = step.descriptionKey
    ? `<div style="color:var(--text-2);">${escapeHtml(I18n.t(step.descriptionKey))}</div>`
    : '';
  return `
    <section data-step="${escapeAttr(step.id)}" style="display:flex;flex-direction:column;gap:10px;padding:12px 0;border-top:1px solid var(--border);">
      <div style="display:flex;align-items:center;gap:8px;">
        <b style="flex:1;">${index + 1}. ${escapeHtml(I18n.t(step.titleKey))}</b>
        <tf-spinner size="sm" data-role="spinner" hidden></tf-spinner>
        <tf-button variant="secondary" size="sm" icon="play" data-role="run">${escapeHtml(I18n.t('addons.install_steps.run'))}</tf-button>
      </div>
      ${description}
      ${(step.fields || []).map(fieldHtml).join('')}
      <div data-role="result"></div>
    </section>`;
}

/** The form's answers as the wire sends them: checkbox `true`/`false`, a
 *  multiselect as its chosen option values joined by commas. */
export function collectStepValues(step, root) {
  const values = [];
  for (const field of step.fields || []) {
    const el = root.querySelector(`[data-field="${CSS.escape(field.id)}"]`);
    if (!el) continue;
    let value;
    if (field.kind === 'checkbox') value = el.checked ? 'true' : 'false';
    else if (field.kind === 'multiselect') value = (el.value || []).join(',');
    else value = String(el.value ?? '').trim();
    values.push([field.id, value]);
  }
  return values;
}

function missingRequired(step, values) {
  const given = new Map(values);
  return (step.fields || []).find((f) => {
    if (!f.required) return false;
    const v = given.get(f.id) ?? '';
    return f.kind === 'checkbox' ? v !== 'true' : v === '';
  });
}

function resultHtml(result) {
  const tone = TONE_OF_STATUS[result.status] || 'danger';
  const details = (result.details || []).map((d) =>
    `<span style="margin-right:12px;"><span style="color:var(--text-2);">${escapeHtml(d.name)}</span> <b>${escapeHtml(d.value)}</b></span>`,
  ).join('');
  return `
    <tf-alert tone="${tone}" message="${escapeAttr(result.message)}"></tf-alert>
    ${details ? `<div style="margin-top:6px;font-size:12px;">${details}</div>` : ''}`;
}

/**
 * Opens the steps window for a freshly installed instance.
 * @param {object} opts
 *   - addonId: the instance the steps run against
 *   - packageName: shown in the title and the incomplete-close notice
 *   - steps: AddonInstallStepInfo[] from the catalog (non-empty)
 *   - onClose: optional callback when the window is dismissed
 * @returns {HTMLElement} the tf-window
 */
export function openInstallSteps({ addonId, packageName, steps, onClose }) {
  // Last outcome per step id in this window; absent = never run.
  const outcomes = new Map();
  const running = new Set();

  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('addons.install_steps.title', { name: packageName }));
  win.setAttribute('icon', 'check');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '420');
  win.setAttribute('width', '560');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `
    <div style="display:flex;flex-direction:column;font-size:13px;">
      <div style="color:var(--text-2);padding-bottom:12px;">${escapeHtml(I18n.t('addons.install_steps.intro'))}</div>
      ${steps.map(stepHtml).join('')}
    </div>`;
  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.innerHTML = `
    <tf-button variant="secondary" icon="play" data-role="run-all">${escapeHtml(I18n.t('addons.install_steps.run_all'))}</tf-button>
    <tf-button variant="primary" icon="check" data-role="finish">${escapeHtml(I18n.t('addons.install_steps.finish'))}</tf-button>
  `;
  win.appendChild(body);
  win.appendChild(foot);

  const block = (step) => body.querySelector(`[data-step="${CSS.escape(step.id)}"]`);

  // The multiselect takes its options as a property, not as children.
  for (const step of steps) {
    for (const field of step.fields || []) {
      if (field.kind !== 'multiselect') continue;
      const el = block(step).querySelector(`[data-field="${CSS.escape(field.id)}"]`);
      el.options = (field.options || []).map((o) => ({ value: o.value, label: I18n.t(o.labelKey) }));
      el.value = String(field.defaultValue || '').split(',').filter(Boolean);
    }
  }

  const paint = (step) => {
    const el = block(step);
    const outcome = outcomes.get(step.id);
    const isRunning = running.has(step.id);
    el.querySelector('[data-role="spinner"]').toggleAttribute('hidden', !isRunning);
    const run = el.querySelector('[data-role="run"]');
    run.toggleAttribute('disabled', isRunning);
    run.textContent = I18n.t(outcome && outcome.status === 'ok' ? 'addons.install_steps.run_again' : outcome ? 'addons.install_steps.retry' : 'addons.install_steps.run');
    el.querySelector('[data-role="result"]').innerHTML = outcome ? resultHtml(outcome) : '';
  };

  /** Runs one step; resolves to its status. Never throws — a transport error is
   *  a failed result like any other. */
  const runStep = async (step) => {
    if (running.has(step.id)) return 'running';
    const values = collectStepValues(step, block(step));
    const missing = missingRequired(step, values);
    if (missing) {
      toast(I18n.t('addons.install_steps.field_required', { field: I18n.t(missing.labelKey) }), 'error');
      return 'invalid';
    }
    running.add(step.id);
    outcomes.delete(step.id);
    paint(step);
    let outcome;
    try {
      const res = await ApiBinary.action('addonInstanceInstallStepRequest', {
        addonId,
        stepId: step.id,
        values,
      });
      outcome = { status: res.status, message: res.message || '', details: res.details || [] };
    } catch (err) {
      outcome = { status: 'failed', message: err.message || String(err), details: [] };
    }
    running.delete(step.id);
    outcomes.set(step.id, outcome);
    paint(step);
    return outcome.status;
  };

  for (const step of steps) {
    block(step).querySelector('[data-role="run"]').addEventListener('click', () => { runStep(step); });
  }

  // Runs the steps that are not green yet, in order, and stops at the first
  // failure: later steps may rely on what an earlier one verified.
  foot.querySelector('[data-role="run-all"]').addEventListener('click', async (e) => {
    const button = e.currentTarget;
    button.setAttribute('disabled', '');
    try {
      for (const step of steps) {
        if (outcomes.get(step.id)?.status === 'ok') continue;
        const status = await runStep(step);
        if (status === 'failed' || status === 'invalid') break;
      }
    } finally {
      button.removeAttribute('disabled');
    }
  });

  const finish = () => {
    const open = steps.filter((s) => outcomes.get(s.id)?.status !== 'ok' && outcomes.get(s.id)?.status !== 'warning');
    if (open.length > 0) {
      toast(I18n.t('addons.install_steps.closed_incomplete', { name: packageName, count: open.length }), 'warning');
    }
    win.remove();
    if (onClose) onClose();
  };
  foot.querySelector('[data-role="finish"]').addEventListener('click', finish);
  win.addEventListener('close-request', (e) => {
    e.preventDefault();
    finish();
  });

  document.body.appendChild(win);
  return win;
}
