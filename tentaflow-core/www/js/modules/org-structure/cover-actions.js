// =============================================================================
// File: modules/org-structure/cover-actions.js
// Description: What the "⋯" menus of absences and deputies do (profile
//   sections and the person menu of the Lista tab): each action opens a window
//   of the shared action module and turns its answer into real org writes.
//   A person adds, changes and deletes their OWN absences; everything else
//   (deputies, other people's absences) is `org.admin` — the server decides,
//   this module only does not offer what it would refuse.
//
//   The window talks in the LAST day of an absence (inclusive); the wire end is
//   exclusive, and cover-model.js converts at this edge.
//
//   Adding and deleting end in a toast whose "Undo" sends the inverse. A change
//   has none: it is a patch of several fields, and taking it back would mean
//   remembering which of them the server accepted.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { dateFormatHint } from '/js/lib/date-format.js';
import { TfToast } from '/js/components/tf-toast.js';
import { openAssignWindow, openConfirmWindow, openEditWindow } from '/js/lib/actions/index.js';
import { orgWrite } from '/js/modules/org-structure/list-actions.js';
import {
  absenceFields, absencePatch, lastDayOf, rangeText, scopeKind,
} from '/js/modules/org-structure/cover-model.js';

const ct = (key, params) => I18n.t(`org_structure.cover.${key}`, params);

const KINDS = ['leave', 'training', 'other'];
const SCOPES = ['all', 'approvals', 'escalations'];

export const kindOptions = () => KINDS.map((value) => ({ value, label: ct(`kind_${value}`) }));
export const scopeOptions = () => SCOPES.map((value) => ({ value, label: ct(`scope_${value}`) }));

/** The label of a wire scope; a project scope carries its id. */
export function scopeLabel(scope) {
  const kind = scopeKind(scope);
  return kind === 'project' ? ct('scope_project') : ct(`scope_${kind}`);
}

export function rangeLabel(item) {
  return rangeText(item.valid_from, item.valid_to, { from: ct('range_from') });
}

// The service answers with a code; the sentence has to fit THIS screen, not the position list's.
const ERROR_KEYS = { duplicate: 'duplicate_deputy', invalid_value: 'invalid_value', invalid_interval: 'invalid_interval' };

async function write(kind, payload) {
  try {
    return await orgWrite(kind, payload);
  } catch (err) {
    const key = ERROR_KEYS[err.code];
    if (key) err.message = ct(`errors.${key}`);
    throw err;
  }
}

/**
 * @param {{ reload: () => Promise<void>, people?: () => Promise<Array<{id: string, name: string}>> }} deps
 *   `reload()` re-reads whatever the caller draws after every write and undo; `people()` lists the accounts
 *   a deputy can be chosen from (needed only by `addDeputy`).
 */
export function createCoverActions({ reload, people = async () => [] }) {
  async function refresh() {
    try {
      await reload();
    } catch {
      // The write already happened; the next reload shows it.
    }
  }

  async function done(body, message, undo) {
    await refresh();
    return undo ? { message, undo } : { message };
  }

  function absenceWindowFields(initial) {
    return [
      { key: 'kind', label: ct('f_kind'), kind: 'select', value: initial.kind, options: kindOptions(), required: true },
      { key: 'from', label: ct('f_from'), kind: 'date', value: initial.from, required: true },
      { key: 'last', label: ct('f_last'), kind: 'date', value: initial.last ?? '', hint: ct('f_last_hint') },
      {
        key: 'reason', label: ct('f_reason'), kind: 'area', value: initial.reason ?? '', maxLength: 500, hint: ct('f_reason_hint'),
      },
    ];
  }

  /** Adds an absence for `userId` (the caller when omitted). */
  function addAbsence({ userId = null, subject, today, anchor = null }) {
    openEditWindow({
      subject,
      title: ct('absence_add_title'),
      submitLabel: ct('absence_add_submit'),
      note: { tone: 'info', text: ct('absence_privacy_note') },
      anchor,
      errorMessage: (err) => err.message,
      fields: absenceWindowFields({ kind: 'leave', from: today }),
      async onSubmit(values) {
        const parsed = absenceFields(values);
        if (parsed.error) throw new Error(ct(`errors.absence_${parsed.error}`, { format: dateFormatHint() }));
        const body = await write('orgAbsenceAddRequest', { userId, ...parsed.fields });
        const created = body.result?.value;
        const undo = async () => {
          await write('orgAbsenceDeleteRequest', { id: created.id, confirmBackdated: body.confirmedBackdated });
          await refresh();
        };
        return done(body, ct('toast_absence_added', { range: rangeLabel(created) }), created ? undo : null);
      },
    });
  }

  function editAbsence(absence, { subject, anchor = null }) {
    openEditWindow({
      subject,
      title: ct('absence_edit_title'),
      submitLabel: ct('absence_edit_submit'),
      note: { tone: 'info', text: ct('absence_privacy_note') },
      anchor,
      errorMessage: (err) => err.message,
      fields: absenceWindowFields({
        kind: absence.kind,
        from: absence.valid_from,
        last: lastDayOf(absence.valid_to),
        reason: absence.reason,
      }),
      async onSubmit(values) {
        const result = absencePatch(absence, values);
        if (result.error) throw new Error(ct(`errors.absence_${result.error}`, { format: dateFormatHint() }));
        if (!result.changed) throw new Error(ct('nothing_changed'));
        const body = await write('orgAbsenceUpdateRequest', { id: absence.id, ...result.patch, clear: result.clear });
        return done(body, ct('toast_absence_edited', { range: rangeLabel(body.result?.value ?? absence) }), null);
      },
    });
  }

  function deleteAbsence(absence, { subject, anchor = null }) {
    openConfirmWindow({
      kind: 'delete',
      title: ct('absence_delete_title'),
      subject,
      consequence: ct('absence_delete_note'),
      submitLabel: ct('absence_delete_submit'),
      anchor,
      errorMessage: (err) => err.message,
      async onSubmit() {
        const body = await write('orgAbsenceDeleteRequest', { id: absence.id });
        // Back: the same absence is added again (a person's own reason travels with it).
        const undo = async () => {
          await write('orgAbsenceAddRequest', {
            userId: absence.user_id,
            validFrom: absence.valid_from,
            validTo: absence.valid_to,
            kind: absence.kind,
            reason: absence.reason,
            confirmBackdated: body.confirmedBackdated,
          });
          await refresh();
        };
        return done(body, ct('toast_absence_deleted', { range: rangeLabel(absence) }), undo);
      },
    });
  }

  /** Appoints a deputy for `userId`: the person themselves or an administrator (the caller checks, the server refuses the rest). */
  async function addDeputy({ userId, userName, today, anchor = null }) {
    let candidates;
    try {
      candidates = (await people()).filter((p) => p.id !== userId);
    } catch (err) {
      TfToast.show({ tone: 'danger', message: ct('people_failed', { message: err.message || '' }) });
      return;
    }
    openAssignWindow({
      subject: userName,
      role: ct('deputy_person'),
      title: ct('deputy_add_title'),
      submitLabel: ct('deputy_add_submit'),
      note: { tone: 'info', text: ct('deputy_note') },
      people: candidates,
      anchor,
      errorMessage: (err) => err.message,
      extraFields: [
        { key: 'scope', label: ct('f_scope'), kind: 'select', value: 'all', options: scopeOptions(), required: true, hint: ct('f_scope_hint') },
        { key: 'from', label: ct('f_from'), kind: 'date', value: today, required: true },
        { key: 'last', label: ct('f_last'), kind: 'date', value: '', hint: ct('f_deputy_last_hint') },
      ],
      async onSubmit({ personId, person, fields }) {
        const parsed = absenceFields({ from: fields.from, last: fields.last, kind: 'other', reason: '' });
        if (parsed.error) throw new Error(ct(`errors.absence_${parsed.error}`, { format: dateFormatHint() }));
        const body = await write('orgDeputySetRequest', {
          userId,
          deputyUserId: personId,
          scope: fields.scope,
          validFrom: parsed.fields.validFrom,
          validTo: parsed.fields.validTo,
        });
        const created = body.result?.value;
        const undo = async () => {
          await write('orgDeputyEndRequest', { id: created.id, from: parsed.fields.validFrom, confirmBackdated: body.confirmedBackdated });
          await refresh();
        };
        return done(body, ct('toast_deputy_added', { deputy: person?.name ?? '', person: userName }), created ? undo : null);
      },
    });
  }

  function editDeputy(deputy, { anchor = null }) {
    openEditWindow({
      subject: ct('deputy_subject', { deputy: deputy.deputy_name, person: deputy.user_name }),
      title: ct('deputy_edit_title'),
      submitLabel: ct('deputy_edit_submit'),
      anchor,
      errorMessage: (err) => err.message,
      fields: [
        {
          key: 'scope',
          label: ct('f_scope'),
          kind: 'select',
          value: deputy.scope,
          // A project scope the server holds stays choosable, so opening the window does not silently change it.
          options: scopeKind(deputy.scope) === 'project'
            ? [...scopeOptions(), { value: deputy.scope, label: scopeLabel(deputy.scope) }]
            : scopeOptions(),
          required: true,
          hint: ct('f_scope_hint'),
        },
        { key: 'from', label: ct('f_from'), kind: 'date', value: deputy.valid_from, required: true },
        { key: 'last', label: ct('f_last'), kind: 'date', value: lastDayOf(deputy.valid_to) ?? '', hint: ct('f_deputy_last_hint') },
      ],
      async onSubmit(values, { changed }) {
        if (!changed.length) throw new Error(ct('nothing_changed'));
        const parsed = absenceFields({ from: values.from, last: values.last, kind: 'other', reason: '' });
        if (parsed.error) throw new Error(ct(`errors.absence_${parsed.error}`, { format: dateFormatHint() }));
        const payload = { id: deputy.id };
        const clear = [];
        if (changed.includes('scope')) payload.scope = values.scope;
        if (changed.includes('from')) payload.validFrom = parsed.fields.validFrom;
        if (changed.includes('last')) {
          if (parsed.fields.validTo) payload.validTo = parsed.fields.validTo;
          else clear.push('valid_to');
        }
        const body = await write('orgDeputyUpdateRequest', { ...payload, clear });
        return done(body, ct('toast_deputy_edited', { deputy: deputy.deputy_name }), null);
      },
    });
  }

  function endDeputy(deputy, { today, anchor = null }) {
    openEditWindow({
      subject: ct('deputy_subject', { deputy: deputy.deputy_name, person: deputy.user_name }),
      title: ct('deputy_end_title'),
      submitLabel: ct('deputy_end_submit'),
      note: { tone: 'warning', text: ct('deputy_end_note') },
      anchor,
      errorMessage: (err) => err.message,
      fields: [{ key: 'from', label: ct('f_end_from'), kind: 'date', value: today, required: true, hint: ct('f_end_from_hint') }],
      async onSubmit(values) {
        const body = await write('orgDeputyEndRequest', { id: deputy.id, from: values.from });
        // Back: appointed again over the same days.
        const undo = async () => {
          await write('orgDeputySetRequest', {
            userId: deputy.user_id,
            deputyUserId: deputy.deputy_user_id,
            scope: deputy.scope,
            validFrom: deputy.valid_from,
            validTo: deputy.valid_to,
            confirmBackdated: body.confirmedBackdated,
          });
          await refresh();
        };
        return done(body, ct('toast_deputy_ended', { deputy: deputy.deputy_name }), undo);
      },
    });
  }

  return { addAbsence, editAbsence, deleteAbsence, addDeputy, editDeputy, endDeputy };
}

/** Members a deputy can be chosen from, read once per screen (open to every member, unlike the user list). */
export function accountPeople() {
  let promise = null;
  return () => {
    promise ??= ApiBinary.one('orgMemberListRequest', {})
      .then((body) => (body.members ?? []).map((m) => ({ id: String(m.user_id), name: m.display_name || I18n.t('org_structure.unknown_person') })))
      .catch((err) => { promise = null; throw err; });
    return promise;
  };
}
