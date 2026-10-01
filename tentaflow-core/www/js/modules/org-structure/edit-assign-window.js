// =============================================================================
// File: modules/org-structure/edit-assign-window.js
// Description: The "assign a person to a position" window of the edit mode. It
//   is the shared form window (lib/actions) with the shared person picker, plus
//   what only the structure has: a person without an account (typed in, created
//   as an external person by the write) and the assignment's own fields — type,
//   share and whether it is the person's main position. The shared
//   openAssignWindow cannot host these, because it insists on a picked person.
// =============================================================================

import { I18n } from '/js/i18n.js';
import '/js/components/tf-person-picker.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-input.js';
import { buildField } from '/js/lib/actions/fields.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { initialsOf } from '/js/modules/org-structure/tree.js';

const t = (key, params) => I18n.t(`org_structure.edit.${key}`, params);
const TYPES = ['permanent', 'acting', 'contractor'];

/**
 * `people` = accounts `{ id, name, email? }`; `externals` = people without an account already in the
 * structure `{ id, name }`. `onSubmit({ person, assignmentType, share, isPrimary })` where `person` is
 * `{ kind: 'user' | 'external', id, name }` or `{ kind: 'new_external', displayName, email }`.
 */
export function openAssignPersonWindow({
  subject, people, externals = [], title, submitLabel, note = null, anchor = null, onSubmit, errorMessage,
}) {
  const items = [
    ...people.map((p) => ({ id: `user:${p.id}`, name: p.name, role: p.email ?? '', initials: initialsOf(p.name), kind: 'person' })),
    ...externals.map((p) => ({ id: `external:${p.id}`, name: p.name, role: t('external_person'), initials: initialsOf(p.name), kind: 'person' })),
  ];
  const picker = document.createElement('tf-person-picker');
  picker.items = items;
  const pickerError = document.createElement('div');
  pickerError.className = 'tf-act__field-error';
  pickerError.hidden = true;
  picker.addEventListener('change', () => { pickerError.hidden = true; });

  const mode = document.createElement('tf-segmented');
  mode.setAttribute('size', 'sm');
  mode.setAttribute('value', 'account');
  mode.setAttribute('aria-label', t('person_source'));
  mode.innerHTML = `<option value="account">${t('person_account')}</option><option value="external">${t('person_external')}</option>`;

  const nameInput = document.createElement('tf-input');
  nameInput.setAttribute('label', `${t('external_name')} *`);
  nameInput.setAttribute('autocomplete', 'off');
  const emailInput = document.createElement('tf-input');
  emailInput.setAttribute('label', t('external_email'));
  emailInput.setAttribute('type', 'email');
  emailInput.setAttribute('autocomplete', 'off');
  const external = document.createElement('div');
  external.className = 'tf-act__fields';
  external.hidden = true;
  external.append(nameInput, emailInput);
  nameInput.addEventListener('input', () => nameInput.removeAttribute('error'));

  mode.addEventListener('change', (e) => {
    e.stopPropagation();
    const isExternal = e.detail.value === 'external';
    picker.hidden = isExternal;
    external.hidden = !isExternal;
    pickerError.hidden = true;
    if (isExternal) nameInput.focus();
    else picker.focusSearch();
  });

  const fields = [
    buildField({ key: 'assignmentType', label: t('assignment_type'), kind: 'select', required: true, value: 'permanent',
      options: TYPES.map((value) => ({ value, label: t(`type_${value}`) })) }, {}),
    buildField({ key: 'share', label: t('assignment_share'), kind: 'number', required: true, value: 1, min: 0.05, max: 1, step: 0.05 }, {}),
    buildField({ key: 'isPrimary', label: t('assignment_primary'), kind: 'select', value: 'auto',
      options: [
        { value: 'auto', label: t('primary_auto') },
        { value: 'yes', label: t('primary_yes') },
        { value: 'no', label: t('primary_no') },
      ] }, {}),
  ];
  const grid = document.createElement('div');
  grid.className = 'tf-act__fields';
  grid.append(...fields.map((f) => f.el));

  const isExternal = () => mode.value === 'external';
  return openFormWindow({
    title, icon: 'user', subject, note, width: 600,
    sections: [mode, picker, pickerError, external, grid],
    fields,
    submitLabel, anchor, errorMessage,
    validate() {
      if (isExternal()) {
        if (nameInput.value.trim()) return true;
        nameInput.setAttribute('error', t('name_required'));
        nameInput.focus();
        return false;
      }
      if (picker.value) return true;
      pickerError.textContent = t('person_required');
      pickerError.hidden = false;
      picker.focusSearch();
      return false;
    },
    collect() {
      const values = Object.fromEntries(fields.map((f) => [f.key, f.read()]));
      let person;
      if (isExternal()) {
        person = { kind: 'new_external', displayName: nameInput.value.trim(), email: emailInput.value.trim() || null, name: nameInput.value.trim() };
      } else {
        const chosen = picker.selectedItems[0];
        const [kind, ...rest] = String(picker.value).split(':');
        person = { kind, id: rest.join(':'), name: chosen?.name ?? '' };
      }
      return {
        person,
        assignmentType: values.assignmentType ?? 'permanent',
        share: values.share ?? 1,
        isPrimary: values.isPrimary === 'yes' ? true : values.isPrimary === 'no' ? false : null,
      };
    },
    onSubmit,
  });
}
