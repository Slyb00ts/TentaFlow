// ===== File: modules/sla/policy-editor.js — the "Edytuj politykę SLA" window (create and edit) =====
//
// Built from the shared action window (`openFormWindow`) and its fields, like the
// other editing windows of the dashboard. Rules the server enforces are checked
// before sending (range, whole numbers, the limit and its window together); what
// only the server can know (a taken name, a value out of range after all) comes
// back as a coded error and is shown under the field it names.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { buildField } from '/js/lib/actions/fields.js';
import { CALL_WINDOWS, FIELD_RANGES, SlaRefusal, T, describeSlaError, failureText, unlessRefused, windowName } from './model.js';

const NAME_MAX_CHARS = 80;

const NUMBER_FIELDS = [
  ['max_concurrency', 'maxConcurrency'],
  ['max_queue', 'maxQueue'],
  ['queue_timeout_ms', 'queueTimeoutMs'],
  ['call_limit', 'callLimit'],
  null, // the call window select sits here
  ['warn_at_pct', 'warnAtPct'],
  ['timeout_ms', 'timeoutMs'],
  ['latency_p95_ms', 'latencyP95Ms'],
  ['error_rate_pct', 'errorRatePct'],
];

const HINTED = new Set(['max_concurrency', 'max_queue', 'queue_timeout_ms', 'call_limit']);
const WIDE = new Set(['name', 'description']);

function numberDefinition(wire, camel, spec) {
  const [min, max] = FIELD_RANGES[wire];
  return {
    key: wire,
    label: T(`editor.field.${wire}`),
    kind: 'number',
    min,
    // The server's upper bound of an unbounded count is the u32 limit: not worth printing.
    ...(max < 4294967295 ? { max } : {}),
    value: spec[camel] ?? '',
    ...(HINTED.has(wire) ? { hint: T(`editor.hint.${wire}`) } : {}),
  };
}

/** The field definitions of the editor for `spec` (the policy's current values, or empty ones). */
export function editorFields(spec = {}) {
  const definitions = [
    { key: 'name', label: T('editor.field.name'), kind: 'text', required: true, value: spec.name ?? '', maxLength: NAME_MAX_CHARS },
    { key: 'description', label: T('editor.field.description'), kind: 'area', value: spec.description ?? '' },
  ];
  for (const entry of NUMBER_FIELDS) {
    if (entry) {
      definitions.push(numberDefinition(entry[0], entry[1], spec));
    } else {
      definitions.push({
        key: 'call_window',
        label: T('editor.field.call_window'),
        kind: 'select',
        value: spec.callWindowSecs == null ? '' : String(spec.callWindowSecs),
        options: CALL_WINDOWS.map((secs) => ({ value: String(secs), label: windowName(secs) })),
        hint: T('editor.hint.call_window'),
      });
    }
  }
  definitions.push({
    key: 'ordering',
    label: T('editor.field.ordering'),
    kind: 'select',
    value: spec.ordering ?? 'partition',
    options: [{ value: 'partition', label: T('editor.ordering_partition') }, { value: 'none', label: T('editor.ordering_none') }],
    hint: T('editor.hint.ordering'),
  });
  return definitions;
}

/** The spec the request carries, from the editor's values by field key. */
export function specFromValues(values) {
  const spec = { name: values.name, description: values.description, ordering: values.ordering || 'partition' };
  for (const entry of NUMBER_FIELDS) {
    if (entry) spec[entry[1]] = values[entry[0]];
  }
  spec.callWindowSecs = values.call_window == null ? null : Number(values.call_window);
  return spec;
}

/**
 * The rule across fields: a call limit and its window go together. Returns the
 * `{ field, message }` that breaks it, or `null`.
 */
export function limitAndWindowProblem(values) {
  if (values.call_limit != null && values.call_window == null) return { field: 'call_window', message: T('error.window_required') };
  if (values.call_limit == null && values.call_window != null) return { field: 'call_limit', message: T('error.limit_required') };
  return null;
}

/**
 * Opens the editor. `policy` is the `SlaPolicyWire` being edited (`null` creates one);
 * `onSaved(response)` runs after the server accepted the write.
 */
export function openPolicyEditor({ policy = null, onSaved = () => {}, anchor = null }) {
  const definitions = editorFields(policy?.spec ?? {});
  const fields = definitions.map((definition) => buildField(definition));
  const byKey = new Map(fields.map((field) => [field.key, field]));
  const grid = document.createElement('div');
  grid.className = 'tf-act__fields';
  grid.style.cssText = 'display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px';
  for (const field of fields) {
    if (WIDE.has(field.key)) field.el.style.gridColumn = '1 / -1';
    grid.append(field.el);
  }
  const readAll = () => Object.fromEntries(fields.map((field) => [field.key, field.read()]));

  const markWholeNumbers = () => {
    let ok = true;
    for (const entry of NUMBER_FIELDS) {
      if (!entry) continue;
      const value = byKey.get(entry[0]).read();
      if (value !== null && Number.isFinite(value) && !Number.isInteger(value)) {
        byKey.get(entry[0]).markError(T('error.integer'));
        ok = false;
      }
    }
    return ok;
  };

  const assigned = policy?.assignedFlows ?? 0;
  const win = openFormWindow({
    title: T(policy ? 'editor.title_edit' : 'editor.title_new'),
    icon: 'shield',
    subject: policy
      ? T(assigned > 0 ? 'editor.subject_assigned' : 'editor.subject_unassigned', { name: policy.spec.name, count: assigned })
      : null,
    note: { tone: 'info', text: T('editor.note') },
    width: 720,
    sections: [grid],
    fields,
    validate: () => {
      const whole = markWholeNumbers();
      const problem = limitAndWindowProblem(readAll());
      if (problem) byKey.get(problem.field).markError(problem.message);
      if (problem) byKey.get(problem.field).focus();
      return whole && !problem;
    },
    collect: () => specFromValues(readAll()),
    submitLabel: T('editor.submit'),
    errorMessage: (err) => {
      if (!(err instanceof SlaRefusal)) return failureText(err);
      const { message, field } = describeSlaError(err.slaError);
      const target = field ? byKey.get(field) : null;
      if (!target) return message;
      target.markError(message);
      target.focus();
      return '';
    },
    onSubmit: async (spec) => {
      const response = unlessRefused(await ApiBinary.action('slaPolicyUpsertRequest', { policyId: policy?.id ?? null, spec }));
      onSaved(response);
      return { message: T(response.created ? 'saved_created' : 'saved_updated', { name: response.policy?.spec?.name ?? spec.name }) };
    },
    anchor,
  });
  win.id = 'sla-editor';
  return win;
}
