// ===== File: modules/services-limits.js — the "Limity" section of the service edit window =====
//
// Two limits of the requests to a service: how many may run at once on the node
// (`max_concurrent`) and how long one waits for a free slot (`timeout_ms`). They
// apply at once, without restarting the service. The rules here are pure, so the
// window only reads the fields, asks for the plan and draws the answer.

import { I18n } from '/js/i18n.js';

export const LIMIT_RANGES = { max_concurrent: [1, 1024], timeout_ms: [100, 86_400_000] };

const tk = (key, vars) => I18n.t(`services_edit.${key}`, vars);
const escapeHtml = (s) => String(s ?? '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');

const positive = (value) => (Number.isInteger(Number(value)) && Number(value) > 0 ? Number(value) : null);

/** The limits the service has now, from its stored config; `null` for "no limit" (0 or absent). */
export function limitsFromConfig(cfg) {
  return { maxConcurrent: positive(cfg?.max_concurrent), timeoutMs: positive(cfg?.timeout_ms) };
}

/**
 * Whether the node hosting the service can change limits: the service row's `limitsEditable`.
 * Only an explicit `false` (a node from before the feature) says no; a row that does not carry
 * the flag at all is left editable and the server's answer decides.
 */
export const limitsEditable = (svc) => svc?.limitsEditable !== false;

/**
 * The section's markup, filled from `initial` (see `limitsFromConfig`). With `editable: false`
 * the fields are disabled and a note says why; the window then sends no limits.
 */
export function renderLimitsSection(initial, { editable = true } = {}) {
  const field = (key, attr, label, value, extra) => `
    <div class="form-row">
      <label for="svc-limit-${attr}">${escapeHtml(label)}</label>
      <input class="tf-input mono" id="svc-limit-${attr}" type="number" inputmode="numeric" data-limit="${key}" value="${escapeHtml(value ?? '')}" ${extra}${editable ? '' : ' disabled'}
        aria-describedby="svc-limit-${attr}-error">
      <div id="svc-limit-${attr}-error" data-limit-error="${key}" role="alert" hidden style="color:var(--danger);font-size:11.5px;margin-top:4px"></div>
    </div>`;
  return `
    <div class="body-section" id="limity">
      <div class="body-section-title">${escapeHtml(tk('limits_title'))}</div>
      <div class="adv-row-2">
        ${field('max_concurrent', 'threads', tk('limits_threads'), initial.maxConcurrent, `min="1" max="1024" placeholder="${escapeHtml(tk('limits_threads_placeholder'))}"`)}
        ${field('timeout_ms', 'timeout', tk('limits_timeout'), initial.timeoutMs, 'min="100"')}
      </div>
      ${editable ? '' : `<div class="form-hint" data-limits-old-node role="note" style="color:var(--warning, var(--text-2))">${escapeHtml(tk('limits_old_node'))}</div>`}
      <div class="form-hint">${escapeHtml(tk('limits_hint_threads'))}</div>
      <div class="form-hint">${escapeHtml(tk('limits_hint_apply'))}</div>
    </div>`;
}

/** The field texts of a service whose limits are left as they are. */
export const initialValues = (initial) => ({ max_concurrent: initial.maxConcurrent == null ? '' : String(initial.maxConcurrent), timeout_ms: initial.timeoutMs == null ? '' : String(initial.timeoutMs) });

/** The raw text of both fields. */
export function readLimits(root) {
  return {
    max_concurrent: root.querySelector('[data-limit="max_concurrent"]')?.value.trim() ?? '',
    timeout_ms: root.querySelector('[data-limit="timeout_ms"]')?.value.trim() ?? '',
  };
}

/**
 * `{ field: message }` for every field that breaks its range, with the same ranges the server
 * enforces. An empty thread limit is fine (no limit); an empty timeout is not, once it had a value.
 */
export function validateLimits(values, initial) {
  const problems = {};
  const whole = (text) => /^\d+$/.test(text);
  const [tMin, tMax] = LIMIT_RANGES.max_concurrent;
  const threads = values.max_concurrent;
  if (threads !== '' && (!whole(threads) || Number(threads) < tMin || Number(threads) > tMax)) {
    problems.max_concurrent = tk('limits_error_threads', { min: tMin, max: tMax });
  }
  const [wMin, wMax] = LIMIT_RANGES.timeout_ms;
  const timeout = values.timeout_ms;
  const required = timeout === '' && initial.timeoutMs != null;
  if (required || (timeout !== '' && (!whole(timeout) || Number(timeout) < wMin || Number(timeout) > wMax))) {
    problems.timeout_ms = tk('limits_error_timeout', { min: new Intl.NumberFormat(I18n.getLanguage()).format(wMin), max: new Intl.NumberFormat(I18n.getLanguage()).format(wMax) });
  }
  return problems;
}

/**
 * Only the limits that changed, as the request names them. An emptied thread limit that
 * had a value is sent as `0` (clears it); one that was empty and stays empty is not sent.
 */
export function limitsDelta(initial, values) {
  const delta = {};
  const threads = values.max_concurrent === '' ? null : Number(values.max_concurrent);
  if (threads !== initial.maxConcurrent) delta.maxConcurrent = threads ?? 0;
  const timeout = values.timeout_ms === '' ? null : Number(values.timeout_ms);
  if (timeout !== initial.timeoutMs && timeout != null) delta.timeoutMs = timeout;
  return delta;
}

const UNCHANGED = {
  modelRepo: null, modelPresetId: null, gpuMemoryUtilization: null, maxModelLen: null, maxNumSeqs: null,
  maxNumBatchedTokens: null, kvCacheDtype: null, chunkedPrefill: null, vllmArgsOverride: null, pinned: null, paused: null,
};

/**
 * What to send. `full` is the window's whole payload now, `baseline` the one it opened with.
 * When nothing but the limits differs, the request carries only the limits (the server then
 * saves them without a restart); otherwise the limits that changed ride along with the rest.
 */
export function savePlan({ full, baseline, delta }) {
  const others = (payload) => JSON.stringify({ ...payload, serviceId: 0, maxConcurrent: undefined, timeoutMs: undefined });
  const limitsOnly = Object.keys(delta).length > 0 && others(full) === others(baseline);
  if (limitsOnly) {
    return { limitsOnly, payload: { serviceId: full.serviceId, nodeId: full.nodeId, ...UNCHANGED, ...delta, restartAfterSave: false } };
  }
  return { limitsOnly, payload: { ...full, ...delta } };
}

/**
 * The localized text of a refusal of the limits. The server's English text is only used to
 * find which field it is about; `field` is `null` when it names none.
 */
export function limitRefusal(err) {
  const code = typeof err?.code === 'string' ? err.code.toLowerCase() : err?.code;
  if (code !== 10 && code !== 'bad_request' && code !== 'badrequest') return null;
  const text = String(err?.message ?? '');
  if (/too old/i.test(text)) return { field: null, message: tk('limits_old_node') };
  const field = ['max_concurrent', 'timeout_ms'].find((name) => text.includes(name)) ?? null;
  const [min, max] = LIMIT_RANGES[field ?? 'max_concurrent'];
  if (field === 'max_concurrent') return { field, message: tk('limits_error_threads', { min, max }) };
  if (field === 'timeout_ms') {
    const fmt = (n) => new Intl.NumberFormat(I18n.getLanguage()).format(n);
    return { field, message: tk('limits_error_timeout', { min: fmt(LIMIT_RANGES.timeout_ms[0]), max: fmt(LIMIT_RANGES.timeout_ms[1]) }) };
  }
  return { field: null, message: tk('limits_error_server') };
}

/** Shows `problems` (`{ field: message }`) against the fields and clears the others; returns the first field with a problem. */
export function showLimitProblems(root, problems) {
  let first = null;
  for (const field of Object.keys(LIMIT_RANGES)) {
    const box = root.querySelector(`[data-limit-error="${field}"]`);
    const input = root.querySelector(`[data-limit="${field}"]`);
    const message = problems[field] ?? '';
    if (box) { box.textContent = message; box.hidden = !message; }
    if (input) {
      if (message) input.setAttribute('aria-invalid', 'true'); else input.removeAttribute('aria-invalid');
    }
    if (message && !first) first = input;
  }
  return first;
}
