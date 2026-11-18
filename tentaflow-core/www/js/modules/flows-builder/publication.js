// ===== File: modules/flows-builder/publication.js — "Ustawienia przepływu": offer a flow as a /v1 model =====
//
// A flow with a publish name is a model of the /v1 API: callers name it in the `model` field. The server
// trims the name, refuses one that another flow or an active alias already holds, and unpublishes on "".
// The rules here are pure; `openPublicationWindow` is the window of the mockups (2c, 2d).

import { I18n } from '/js/i18n.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { buildField } from '/js/lib/actions/fields.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-input.js';
import { readFlowList } from '/js/modules/sla/flow-list.js';

export const MODEL_NAME_MAX = 64;

const T = (key, vars) => I18n.t(`flow_publication.${key}`, vars);

/** `POST /v1/chat/completions · model: <name>`: what an application calls. */
export const callAddress = (name) => `POST /v1/chat/completions · model: ${name}`;

/**
 * What is wrong with `text` as a model name, or `null`. The server accepts any non-blank name, so
 * this only keeps out what cannot be a model id in a JSON field: nothing, too long, control characters.
 */
export function modelNameProblem(text) {
  const name = String(text ?? '').trim();
  if (!name) return 'required';
  if ([...name].length > MODEL_NAME_MAX) return 'too_long';
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(name)) return 'characters';
  return null;
}

/**
 * The body fields of the update, or `null` when nothing changes. `initial` is the name the flow has
 * now (`''`/`null` = not published). Off sends `""`, which the server reads as "unpublish".
 */
export function publicationChange(initial, { enabled, name }) {
  const current = (initial ?? '').trim();
  if (!enabled) return current ? { publishedModelName: '' } : null;
  const next = String(name ?? '').trim();
  return next === current ? null : { publishedModelName: next };
}

/**
 * Classifies a refusal of the publish name: `{ kind: 'flow' | 'alias' }`, or `null` for anything else.
 * The server's text is only read to tell the cases apart and is never shown.
 */
export function nameClash(err) {
  const code = typeof err?.code === 'string' ? err.code.toLowerCase() : err?.code;
  const text = String(err?.message ?? '');
  if (!/flow publish name/i.test(text)) return null;
  if (code != null && code !== 10 && code !== 'bad_request' && code !== 'badrequest') return null;
  if (/active alias/i.test(text)) return { kind: 'alias' };
  // "… is already used by flow 'X'": the server may name the other flow; the current one does not.
  const named = /(?:by|another) flow\s+['"“„‘]([^'"”’]+)['"”’]/i.exec(text);
  return { kind: 'flow', flowName: named ? named[1] : null };
}

/** The localized field error for a clash. `flowName` is the flow holding the name when it is known. */
export function clashMessage(clash, flowName = null) {
  if (clash.kind === 'alias') return T('error.alias');
  const holder = flowName ?? clash.flowName;
  return holder ? T('error.taken_by', { flow: holder }) : T('error.taken');
}

/** The name of the other flow that publishes `name`, from the flow list; `null` when unknown. */
async function holderOf(name, flowId, readList) {
  try {
    const response = await readList();
    const flow = (response.flows ?? []).find((f) => f.id !== flowId && f.publishedModelName === name);
    return flow?.name ?? null;
  } catch {
    return null;
  }
}

/** The name a flow is published under now, from the flow list; `null` when none (or when it cannot be read). */
export async function currentPublication(flowId, readList = readFlowList) {
  const response = await readList();
  const flow = (response.flows ?? []).find((f) => f.id === flowId);
  return flow?.publishedModelName?.trim() || null;
}

/**
 * Opens the window for `flow` (`{ id, name }`) published as `current` (`null` = not published).
 * `onSaved({ publishedModelName })` runs after the server accepted the change; `deps` is for tests.
 */
export function openPublicationWindow({ flow, current, onSaved = () => {}, anchor = null, deps = {} }) {
  const call = deps.call ?? ((payload) => ApiBinary.action('flowUpdateRequest', payload));
  const readList = deps.readList ?? readFlowList;

  const toggle = document.createElement('tf-toggle');
  toggle.setAttribute('label', T('toggle'));
  if (current) toggle.setAttribute('checked', '');
  const toggleField = document.createElement('div');
  toggleField.className = 'tf-act__field';
  toggleField.append(toggle);

  const nameField = buildField({
    key: 'model',
    label: T('name'),
    kind: 'text',
    value: current ?? '',
    placeholder: T('name_placeholder'),
    hint: T('name_hint'),
    maxLength: MODEL_NAME_MAX,
  });
  const nameControl = nameField.el.querySelector('tf-input');

  const address = document.createElement('tf-input');
  address.setAttribute('label', T('address'));
  address.setAttribute('readonly', '');
  const addressField = document.createElement('div');
  addressField.className = 'tf-act__field';
  addressField.append(address);

  const enabled = () => toggle.hasAttribute('checked');
  const refresh = () => {
    nameControl.toggleAttribute('disabled', !enabled());
    const name = nameField.read();
    address.setAttribute('value', enabled() && name ? callAddress(name) : '');
  };
  refresh();
  toggle.addEventListener('change', () => { nameField.markError(''); refresh(); });
  nameControl.addEventListener('input', refresh);

  const section = document.createElement('div');
  section.append(toggleField, nameField.el, addressField);

  return openFormWindow({
    title: T('title'),
    icon: 'globe',
    subject: T('subject', { name: flow.name }),
    sections: [section],
    fields: [],
    validate: () => {
      if (!enabled()) return true;
      const problem = modelNameProblem(nameField.read());
      if (problem) {
        nameField.markError(T(`error.${problem}`, { max: MODEL_NAME_MAX }));
        nameField.focus();
        return false;
      }
      return true;
    },
    collect: () => ({ enabled: enabled(), name: nameField.read() }),
    submitLabel: T('submit'),
    errorMessage: (err) => (err?.onField ? '' : T('error.generic')),
    onSubmit: async (values) => {
      const change = publicationChange(current, values);
      if (!change) return { message: T('saved_unchanged') };
      try {
        await call({ flowId: flow.id, ...change });
      } catch (err) {
        const clash = nameClash(err);
        if (!clash) throw err;
        const holder = clash.kind === 'flow' ? await holderOf(values.name.trim(), flow.id, readList) : null;
        nameField.markError(clashMessage(clash, holder));
        nameField.focus();
        // The window keeps its error line empty: the refusal is shown on the field.
        throw Object.assign(new Error(''), { onField: true });
      }
      onSaved({ publishedModelName: change.publishedModelName || null });
      return { message: change.publishedModelName ? T('saved_published', { name: change.publishedModelName }) : T('saved_unpublished') };
    },
    anchor,
  });
}
