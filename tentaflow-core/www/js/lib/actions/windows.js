// ===== File: lib/actions/windows.js — assign, hand over, edit, move and confirm windows =====
//
// The five windows behind the "⋯" menu of every changeable item. They differ in
// what they ask for; the modal, validation, busy state and error handling are
// the shared form window's. Data (people, targets, fields) always comes from
// the caller; nothing here knows what an item is.
//
// Assign vs reassign vs hand over (PROJECT_STUDIO_WORKFLOW_PLAN.md §4.5):
//   assign / reassign — fills or changes the person; the previous one is just
//     informed. No explanation is owed.
//   hand over — the person changes AND a comment for the receiver is REQUIRED,
//     because the receiver starts from what the previous person left. The
//     window offers what happens to the giver (stay as watcher, move the running
//     time tracking) and whether to notify.

import '/js/components/tf-person-picker.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-radio.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-input.js';
import { buildField, t } from './fields.js';
import { openFormWindow } from './form-window.js';

const MANY_TARGETS = 10;

function fieldBlock(label, control) {
  const el = document.createElement('div');
  el.className = 'tf-act__field tf-act__field--wide';
  const caption = document.createElement('div');
  caption.className = 'tf-act__label';
  caption.textContent = label;
  const error = document.createElement('div');
  error.className = 'tf-act__field-error';
  error.hidden = true;
  el.append(caption, control, error);
  return {
    el,
    setError(msg) { error.textContent = msg; error.hidden = !msg; },
  };
}

// The picker plus its label and error line. `required` = a choice must be made.
function personBlock({ label, people, agents, allowAgents }) {
  const picker = document.createElement('tf-person-picker');
  picker.items = [
    ...people.map((p) => ({ ...p, kind: 'person' })),
    ...(allowAgents ? agents.map((a) => ({ ...a, kind: 'agent' })) : []),
  ];
  const block = fieldBlock(label, picker);
  picker.addEventListener('change', () => block.setError(''));
  return {
    el: block.el,
    picker,
    validate() {
      if (picker.value) return true;
      block.setError(t('person_required'));
      picker.focusSearch();
      return false;
    },
  };
}

function checkboxes(defs) {
  const el = document.createElement('div');
  el.className = 'tf-act__checks';
  const boxes = {};
  for (const { key, label, checked } of defs) {
    const box = document.createElement('tf-checkbox');
    box.setAttribute('label', label);
    if (checked) box.setAttribute('checked', '');
    boxes[key] = box;
    el.appendChild(box);
  }
  return { el, read: () => Object.fromEntries(Object.entries(boxes).map(([k, b]) => [k, b.checked])) };
}

function submitOnActivate(getWin) {
  return () => getWin().querySelector('[data-act="submit"]').click();
}

/**
 * Assign or reassign a person (or agent) to a role of `subject`.
 * `people` / `agents` = tf-person-picker items; agents are offered only when
 * `allowAgents`. `extraFields` (same shape as the edit window) ask for more —
 * an allocation's share and period. `notify` (true/false) adds a "notify"
 * checkbox with that default; leave it out when the caller decides.
 * `onSubmit({ personId, person, fields, notify? })`.
 */
export function openAssignWindow({
  subject, role = null, people = [], agents = [], allowAgents = false, extraFields = [], notify,
  title = t('assign.title'), submitLabel = t('assign.submit'), note = null, anchor = null,
  onSubmit, errorMessage,
}) {
  const block = personBlock({ label: role ?? t('assign.person_label'), people, agents, allowAgents });
  const extras = extraFields.map((f) => buildField(f, { people }));
  const grid = document.createElement('div');
  grid.className = 'tf-act__fields';
  grid.append(...extras.map((f) => f.el));
  const notifyBox = notify === undefined ? null : checkboxes([{ key: 'notify', label: t('assign.notify'), checked: notify }]);
  const sections = [block.el];
  if (extras.length) sections.push(grid);
  if (notifyBox) sections.push(notifyBox.el);
  let win;
  // Enter in the list picks the person; with more fields to fill it moves on to the first of them instead of
  // submitting a window that is not complete yet.
  block.picker.addEventListener('activate', extras.length ? () => extras[0].focus() : submitOnActivate(() => win));
  win = openFormWindow({
    title, icon: 'user', subject, note, sections, fields: extras, validate: block.validate,
    submitLabel, anchor, errorMessage,
    collect: () => ({
      personId: block.picker.value,
      person: block.picker.selectedItems[0],
      fields: Object.fromEntries(extras.map((f) => [f.key, f.read()])),
      ...(notifyBox ? notifyBox.read() : {}),
    }),
    onSubmit,
  });
  return win;
}

/**
 * Hand `subject` over to another person or agent. The comment is REQUIRED; an
 * empty one keeps the window open with an inline message. `options` chooses
 * which extra switches appear and their defaults: `{ stayWatcher, moveTimeTracking,
 * notify }` — a key left out hides its checkbox.
 * `onSubmit({ personId, person, comment, options })`.
 */
export function openHandoverWindow({
  subject, people = [], agents = [], allowAgents = false, options = {},
  title = t('handover.title'), submitLabel = t('handover.submit'), note = { tone: 'info', text: t('handover.note') },
  anchor = null, onSubmit, errorMessage,
}) {
  const block = personBlock({ label: t('handover.person_label'), people, agents, allowAgents });
  const comment = document.createElement('tf-textarea');
  comment.setAttribute('label', `${t('handover.comment_label')} *`);
  comment.setAttribute('placeholder', t('handover.comment_placeholder'));
  comment.setAttribute('rows', '4');
  comment.addEventListener('input', () => comment.removeAttribute('error'));
  const defs = [
    ['stayWatcher', 'handover.stay_watcher'],
    ['moveTimeTracking', 'handover.move_time_tracking'],
    ['notify', 'handover.notify'],
  ].filter(([key]) => key in options).map(([key, label]) => ({ key, label: t(label), checked: Boolean(options[key]) }));
  const switches = defs.length ? checkboxes(defs) : null;
  const sections = [block.el, comment];
  if (switches) sections.push(switches.el);
  block.picker.addEventListener('activate', () => comment.focus());
  const validateComment = () => {
    if (comment.value.trim()) return true;
    comment.setAttribute('error', t('handover.comment_required'));
    return false;
  };
  return openFormWindow({
    title, icon: 'send', subject, note, sections, width: 600,
    validate() {
      const personOk = block.validate();
      const commentOk = validateComment();
      if (personOk && !commentOk) comment.focus();
      return personOk && commentOk;
    },
    submitLabel, submitIcon: 'send', anchor, errorMessage,
    collect: () => ({
      personId: block.picker.value,
      person: block.picker.selectedItems[0],
      comment: comment.value.trim(),
      options: switches ? switches.read() : {},
    }),
    onSubmit,
  });
}

/**
 * Edit the fields of `subject`. `fields` = [{ key, label, kind: text|area|select|
 * date|person|number, value, options, required, hint, placeholder, min, max,
 * step, maxLength }]; `people` feeds the `person` kind ([{ id, name }]).
 * `onSubmit(values, { changed })` — `values` maps every key to its value
 * (empty = null for select, date, person and number), `changed` lists the keys
 * that differ from what the window opened with.
 */
export function openEditWindow({
  subject, fields, people = [], title = t('edit.title'), submitLabel = t('edit.submit'),
  note = null, anchor = null, onSubmit, errorMessage,
}) {
  const built = fields.map((f) => buildField(f, { people }));
  const initial = Object.fromEntries(built.map((f) => [f.key, f.read()]));
  const grid = document.createElement('div');
  grid.className = 'tf-act__fields';
  grid.append(...built.map((f) => f.el));
  return openFormWindow({
    title, icon: 'edit', subject, note, sections: [grid], fields: built, submitLabel, anchor, errorMessage,
    collect: () => Object.fromEntries(built.map((f) => [f.key, f.read()])),
    onSubmit: (values) => onSubmit(values, {
      changed: Object.keys(values).filter((k) => JSON.stringify(values[k]) !== JSON.stringify(initial[k])),
    }),
  });
}

/**
 * Move `subject` to one of `targets` = [{ id, label, hint?, disabled? }] where
 * `disabled` is the reason the target cannot be chosen (shown under it). `selected`
 * preselects a target id. `noteFor(target)` answers a note naming the consequence of
 * that target; it is shown only once a target other than `selected` is chosen.
 * `onSubmit({ targetId, target })`.
 */
export function openMoveWindow({
  subject, targets, selected = null, note = null, noteFor = null, title = t('move.title'), submitLabel = t('move.submit'),
  anchor = null, onSubmit, errorMessage,
}) {
  const sections = [];
  const group = document.createElement('tf-radio-group');
  group.setAttribute('name', 'move-target');
  group.setAttribute('label', t('move.target_label'));
  const radios = new Map();
  for (const target of targets) {
    const radio = document.createElement('tf-radio');
    radio.setAttribute('value', String(target.id));
    radio.setAttribute('label', target.label);
    const hint = target.disabled || target.hint;
    if (hint) radio.setAttribute('hint', hint);
    if (target.disabled) radio.setAttribute('disabled', '');
    radios.set(radio, target);
    group.appendChild(radio);
  }
  if (selected != null) group.setAttribute('value', String(selected));
  const error = document.createElement('div');
  error.className = 'tf-act__field-error';
  error.hidden = true;
  const consequence = document.createElement('tf-alert');
  consequence.className = 'tf-act__note';
  consequence.hidden = true;
  const syncConsequence = () => {
    const chosen = targets.find((tg) => String(tg.id) === group.value);
    const text = noteFor && chosen && String(chosen.id) !== String(selected) ? noteFor(chosen) : null;
    consequence.hidden = !text;
    if (!text) return;
    consequence.setAttribute('tone', text.tone ?? 'warning');
    consequence.setAttribute('message', text.text);
  };
  group.addEventListener('change', () => { error.hidden = true; syncConsequence(); });

  if (targets.length > MANY_TARGETS) {
    const filter = document.createElement('tf-searchbox');
    filter.setAttribute('debounce', '0');
    filter.setAttribute('placeholder', t('move.filter'));
    filter.addEventListener('input', (e) => {
      const q = e.target.value.trim().toLowerCase();
      for (const [radio, target] of radios) radio.toggleAttribute('hidden', Boolean(q) && !target.label.toLowerCase().includes(q));
      group.refresh();
    });
    sections.push(filter);
  }
  if (targets.length) sections.push(group, consequence, error);
  else {
    const empty = document.createElement('tf-alert');
    empty.setAttribute('tone', 'warning');
    empty.setAttribute('message', t('move.empty'));
    sections.push(empty);
  }
  return openFormWindow({
    title, icon: 'arrow', subject, note, sections, submitLabel, anchor, errorMessage,
    canSubmit: () => targets.length > 0,
    validate() {
      if (group.value) return true;
      error.textContent = t('move.target_required');
      error.hidden = false;
      return false;
    },
    collect: () => ({ targetId: group.value, target: targets.find((tg) => String(tg.id) === group.value) }),
    onSubmit,
  });
}

/**
 * Confirm archiving or deleting `subject`. `consequence` says what will happen
 * (shown as the window's note — required, nobody confirms in the dark).
 * `requireReason` adds a required reason field; `confirmPhrase` keeps the
 * button locked until that exact text is typed (for irreversible deletes).
 * `onSubmit({ reason })`.
 */
export function openConfirmWindow({
  kind, subject, consequence, requireReason = false, confirmPhrase = null,
  title, submitLabel, reasonLabel = t('confirm.reason_label'), anchor = null, onSubmit, errorMessage,
}) {
  if (kind !== 'archive' && kind !== 'delete') throw new Error(`unknown confirm kind '${kind}'`);
  const del = kind === 'delete';
  const sections = [];
  const fields = [];
  let reason = null;
  if (requireReason) {
    reason = buildField({ key: 'reason', label: reasonLabel, kind: 'area', required: true }, {});
    fields.push(reason);
    sections.push(reason.el);
  }
  let phraseInput = null;
  if (confirmPhrase) {
    phraseInput = document.createElement('tf-input');
    phraseInput.setAttribute('label', t('confirm.phrase_label', { phrase: confirmPhrase }));
    phraseInput.setAttribute('placeholder', confirmPhrase);
    phraseInput.setAttribute('autocomplete', 'off');
    phraseInput.setAttribute('spellcheck', 'false');
    sections.push(phraseInput);
  }
  return openFormWindow({
    title: title ?? t(del ? 'confirm.delete_title' : 'confirm.archive_title'),
    icon: del ? 'trash' : 'history',
    subject,
    note: { tone: del ? 'danger' : 'warning', text: consequence },
    sections, fields, anchor, errorMessage,
    submitLabel: submitLabel ?? t(del ? 'confirm.delete_submit' : 'confirm.archive_submit'),
    submitVariant: del ? 'danger-solid' : 'primary',
    submitIcon: del ? 'trash' : 'history',
    canSubmit: () => !phraseInput || phraseInput.value === confirmPhrase,
    collect: () => ({ reason: reason ? reason.read() : null }),
    onSubmit,
  });
}
