// =============================================================================
// File: modules/org-structure/list-actions.js
// Description: What the "⋯" menu of the Lista tab (and "Dodaj stanowisko") does:
//   each action opens a window of the shared action module and turns its
//   answer into real org-structure writes (`org*Request`, `org.admin` on the
//   server). A write the server refuses stays in the window as a sentence keyed
//   on the rule's code; a change dated before today asks for confirmation once
//   and repeats the write with `confirmBackdated`.
//
//   Every write ends in a toast whose "Undo" sends the inverse: an edit goes back
//   with the old values from the same day, an end creates the assignment (or
//   position) again from that day, and something created is ended on the day it
//   starts — the structure then removes it instead of leaving an empty interval.
//   The one write with no undo is an edit that also sets an end date: it is two
//   steps of the structure and cannot be taken back as one.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { dateFormatHint, formatDay } from '/js/lib/date-format.js';
import { TfToast } from '/js/components/tf-toast.js';
import {
  openAssignWindow, openConfirmWindow, openEditWindow, openMoveWindow,
} from '/js/lib/actions/index.js';
import { warningText } from '/js/modules/org-structure/model.js';
import { assignmentSnapshot, endConsequences, moveTargets } from '/js/modules/org-structure/list-model.js';

const lt = (key, params) => I18n.t(`org_structure.list.${key}`, params);
const ot = (key, params) => I18n.t(`org_structure.${key}`, params);

const TYPES = ['permanent', 'acting', 'contractor'];

/** The sentence for a write the server refused; a code this build has no sentence for still says which rule. */
export function writeErrorText(error) {
  const code = String(error?.code ?? '');
  const key = `org_structure.list.errors.${code}`;
  const text = I18n.t(key, { format: dateFormatHint() });
  return text && text !== key ? text : lt('error_unknown', { code: code || '—' });
}

function askBackdated(date) {
  return new Promise((resolve) => {
    let confirmed = false;
    const win = openConfirmWindow({
      kind: 'archive',
      title: lt('backdated_title'),
      subject: lt('backdated_subject', { date: formatDay(date) }),
      consequence: lt('backdated_note'),
      submitLabel: lt('backdated_confirm'),
      onSubmit: async () => { confirmed = true; },
    });
    win.addEventListener('closed', () => resolve(confirmed));
  });
}

/**
 * Sends one org write. A refusal for a backdated change asks the administrator once and sends it
 * again confirmed; any other refusal throws an Error whose message is the sentence to show.
 * The answer carries `confirmedBackdated`, so an undo can repeat the same day without asking again.
 */
export async function orgWrite(kind, payload) {
  let body = await ApiBinary.one(kind, payload);
  let confirmed = Boolean(payload.confirmBackdated);
  if (!body.ok && body.error?.code === 'backdated_confirmation_required' && !confirmed) {
    if (!(await askBackdated(body.error.date ?? payload.from ?? payload.validFrom ?? ''))) {
      throw new Error(lt('backdated_declined'));
    }
    confirmed = true;
    body = await ApiBinary.one(kind, { ...payload, confirmBackdated: true });
  }
  if (!body.ok) {
    const err = new Error(writeErrorText(body.error));
    err.code = body.error?.code;
    throw err;
  }
  return { ...body, confirmedBackdated: confirmed };
}

const userSubject = (id) => ({ kind: 'user', id });
const person = (row) => `${row.personName} · ${row.positionName}`;

function typeOptions() {
  return TYPES.map((value) => ({ value, label: lt(`type_${value}`) }));
}

function yesNoOptions() {
  return [{ value: 'true', label: lt('yes') }, { value: 'false', label: lt('no') }];
}

/**
 * @param {{ context: () => { view: object, rows: Array, day: string }, reload: () => Promise<void> }} deps
 *   `context()` is read at the moment an action starts, so a window always works on what is on screen now;
 *   `reload()` re-reads the structure after every write and undo.
 */
export function createListActions({ context, reload }) {
  let peoplePromise = null;

  // Accounts a person can be assigned from; read once per screen, and only when a window needs it.
  function people() {
    peoplePromise ??= ApiBinary.list('usersListRequest', { arrayKey: 'users' }).then((users) => users
      .filter((u) => u.is_active !== false && u.isActive !== false)
      .map((u) => ({ id: String(u.id), name: u.display_name || u.displayName || u.username, role: u.username })))
      .catch((err) => { peoplePromise = null; throw err; });
    return peoplePromise;
  }

  async function refresh() {
    try {
      await reload();
    } catch {
      // The write already happened; the next reload shows it.
    }
  }

  function warn(body) {
    const { view } = context();
    for (const warning of body.warnings ?? []) {
      TfToast.show({ tone: 'warning', message: warningText(warning, view, ot) });
    }
  }

  async function done(body, message, undo) {
    warn(body);
    await refresh();
    return undo ? { message, undo } : { message };
  }

  function editAssignment(row, anchor) {
    const { day } = context();
    openEditWindow({
      subject: person(row),
      title: lt('edit_title'),
      submitLabel: lt('edit_submit'),
      anchor,
      errorMessage: (err) => err.message,
      fields: [
        { key: 'share', label: lt('f_share'), kind: 'number', value: row.share, min: 0.01, max: 1, step: 0.05, required: true, hint: lt('f_share_hint') },
        { key: 'type', label: lt('f_type'), kind: 'select', value: row.type, options: typeOptions(), required: true },
        { key: 'primary', label: lt('f_primary'), kind: 'select', value: String(row.primary), options: yesNoOptions(), required: true },
        { key: 'from', label: lt('f_from'), kind: 'date', value: day, required: true, hint: lt('f_from_hint') },
        { key: 'to', label: lt('f_to'), kind: 'date', value: '', hint: lt('f_to_hint') },
      ],
      async onSubmit(values, { changed }) {
        const patched = ['share', 'type', 'primary'].filter((key) => changed.includes(key));
        if (!patched.length && !values.to) throw new Error(lt('nothing_changed'));
        const patch = {
          assignmentType: patched.includes('type') ? values.type : undefined,
          share: patched.includes('share') ? values.share : undefined,
          isPrimary: patched.includes('primary') ? values.primary === 'true' : undefined,
        };
        const back = {
          assignmentType: patched.includes('type') ? row.type : undefined,
          share: patched.includes('share') ? row.share : undefined,
          isPrimary: patched.includes('primary') ? row.primary : undefined,
        };
        let id = row.assignmentId;
        let updated = null;
        if (patched.length) {
          updated = await orgWrite('orgAssignmentUpdateRequest', { assignmentId: id, ...patch, from: values.from });
          id = updated.result?.value?.id ?? id;
        }
        let ended = null;
        if (values.to) {
          try {
            ended = await orgWrite('orgAssignmentEndRequest', { assignmentId: id, from: values.to });
          } catch (err) {
            // Half of a save must not stay: the changed values go back before the refusal is shown.
            if (updated) {
              await orgWrite('orgAssignmentUpdateRequest', {
                assignmentId: id, ...back, from: values.from, confirmBackdated: updated.confirmedBackdated,
              }).catch(() => {});
            }
            throw err;
          }
        }
        // Ending in the same save cannot be taken back as one step, so that toast has no undo.
        const undo = updated && !ended
          ? async () => {
            await orgWrite('orgAssignmentUpdateRequest', {
              assignmentId: id, ...back, from: values.from, confirmBackdated: updated.confirmedBackdated,
            });
            await refresh();
          }
          : null;
        return done(updated ?? ended, lt('toast_edited', { person: row.personName }), undo);
      },
    });
  }

  function moveAssignment(row, anchor) {
    const { day, rows } = context();
    openMoveWindow({
      subject: person(row),
      title: lt('move_title'),
      submitLabel: lt('move_submit'),
      targets: moveTargets(rows),
      note: { tone: 'info', text: lt('move_note', { day: formatDay(day) }) },
      anchor,
      errorMessage: (err) => err.message,
      async onSubmit({ targetId, target }) {
        // The old seat ends first: when that is refused nothing has changed. If taking the new seat is
        // refused after that, the old assignment is put back so nobody is left without a position.
        const snapshot = assignmentSnapshot(row);
        const ended = await orgWrite('orgAssignmentEndRequest', { assignmentId: row.assignmentId, from: day });
        try {
          const taken = await orgWrite('orgAssignRequest', {
            positionId: targetId,
            subject: row.subject,
            assignmentType: row.type,
            share: row.share,
            isPrimary: row.primary,
            validFrom: day,
            confirmBackdated: ended.confirmedBackdated,
          });
          const newId = taken.result?.value?.id;
          // Back: the new seat goes (it starts today, so it is removed), then the old one is created again.
          const undo = async () => {
            await orgWrite('orgAssignmentEndRequest', { assignmentId: newId, from: day, confirmBackdated: ended.confirmedBackdated });
            await orgWrite('orgAssignRequest', {
              positionId: snapshot.positionId,
              subject: snapshot.subject,
              assignmentType: snapshot.assignmentType,
              share: snapshot.share,
              isPrimary: snapshot.isPrimary,
              validFrom: day,
              validTo: snapshot.validTo,
              confirmBackdated: ended.confirmedBackdated,
            });
            await refresh();
          };
          return done(taken, lt('toast_moved', { person: row.personName, target: target.label }), undo);
        } catch (err) {
          await orgWrite('orgAssignRequest', {
            positionId: snapshot.positionId,
            subject: snapshot.subject,
            assignmentType: snapshot.assignmentType,
            share: snapshot.share,
            isPrimary: snapshot.isPrimary,
            validFrom: day,
            validTo: snapshot.validTo,
            confirmBackdated: ended.confirmedBackdated,
          }).catch(() => {});
          throw err;
        }
      },
    });
  }

  function endAssignment(row, anchor) {
    const { day, view, rows } = context();
    const consequences = endConsequences(row, view, rows).map((c) => lt(`consequence_${c.key}`, c.params));
    openEditWindow({
      subject: person(row),
      title: lt('end_title'),
      submitLabel: lt('end_submit'),
      note: { tone: 'warning', text: [...consequences, lt('end_no_handover')].join(' ') },
      anchor,
      errorMessage: (err) => err.message,
      fields: [{ key: 'from', label: lt('f_end_from'), kind: 'date', value: day, required: true, hint: lt('f_end_from_hint') }],
      async onSubmit(values) {
        const snapshot = assignmentSnapshot(row);
        const ended = await orgWrite('orgAssignmentEndRequest', { assignmentId: row.assignmentId, from: values.from });
        const undo = async () => {
          await orgWrite('orgAssignRequest', {
            positionId: snapshot.positionId,
            subject: snapshot.subject,
            assignmentType: snapshot.assignmentType,
            share: snapshot.share,
            isPrimary: snapshot.isPrimary,
            validFrom: values.from,
            validTo: snapshot.validTo,
            confirmBackdated: ended.confirmedBackdated,
          });
          await refresh();
        };
        return done(ended, lt('toast_ended', { person: row.personName, date: formatDay(values.from) }), undo);
      },
    });
  }

  async function assignPerson(row, anchor) {
    const { day } = context();
    let candidates;
    try {
      candidates = await people();
    } catch (err) {
      TfToast.show({ tone: 'danger', message: lt('people_failed', { message: err.message || '' }) });
      return;
    }
    openAssignWindow({
      subject: `${row.positionName} · ${row.unitName}`,
      role: lt('assign_person'),
      title: lt('assign_title'),
      submitLabel: lt('assign_submit'),
      people: candidates,
      anchor,
      errorMessage: (err) => err.message,
      extraFields: [
        { key: 'share', label: lt('f_share'), kind: 'number', value: 1, min: 0.01, max: 1, step: 0.05, required: true, hint: lt('f_share_hint') },
        { key: 'type', label: lt('f_type'), kind: 'select', value: 'permanent', options: typeOptions(), required: true },
        { key: 'from', label: lt('f_assign_from'), kind: 'date', value: day, required: true },
      ],
      async onSubmit({ personId, person: picked, fields }) {
        const taken = await orgWrite('orgAssignRequest', {
          positionId: row.positionId,
          subject: userSubject(personId),
          assignmentType: fields.type,
          share: fields.share,
          validFrom: fields.from,
        });
        const assignmentId = taken.result?.value?.id;
        const undo = async () => {
          await orgWrite('orgAssignmentEndRequest', {
            assignmentId, from: fields.from, confirmBackdated: taken.confirmedBackdated,
          });
          await refresh();
        };
        return done(taken, lt('toast_assigned', { person: picked?.name ?? '', position: row.positionName }), undo);
      },
    });
  }

  function endPosition(row, anchor) {
    const { day } = context();
    openEditWindow({
      subject: `${row.positionName} · ${row.unitName}`,
      title: lt('end_position_title'),
      submitLabel: lt('end_position_submit'),
      note: { tone: 'warning', text: lt('end_position_note') },
      anchor,
      errorMessage: (err) => err.message,
      fields: [{ key: 'from', label: lt('f_end_from'), kind: 'date', value: day, required: true, hint: lt('f_end_position_from_hint') }],
      async onSubmit(values) {
        const ended = await orgWrite('orgPositionEndRequest', { positionId: row.positionId, from: values.from });
        // Back: the position is created again from that day, in its unit, under its manager.
        const undo = async () => {
          await orgWrite('orgPositionCreateRequest', {
            unitId: row.unitId,
            name: row.positionName,
            parentPositionId: row.parentPositionId,
            isStaff: row.staff,
            validFrom: values.from,
            confirmBackdated: ended.confirmedBackdated,
          });
          await refresh();
        };
        return done(ended, lt('toast_position_ended', { position: row.positionName, date: formatDay(values.from) }), undo);
      },
    });
  }

  async function addPosition(anchor) {
    const { day, view, rows } = context();
    let candidates;
    try {
      candidates = await people();
    } catch (err) {
      TfToast.show({ tone: 'danger', message: lt('people_failed', { message: err.message || '' }) });
      return;
    }
    const units = (view.units ?? []).map((u) => ({ value: u.unit_id, label: u.name }));
    // A staff position cannot manage, so it is never offered as somebody's manager.
    const parents = (view.positions ?? []).filter((p) => !p.is_staff).map((p) => {
      const holders = rows.filter((r) => r.positionId === p.position_id && !r.vacant).map((r) => r.personName);
      const who = holders.length ? holders.join(', ') : ot('vacancy');
      const unit = units.find((u) => u.value === p.unit_id)?.label ?? '';
      return { value: p.position_id, label: `${p.name} — ${who} (${unit})` };
    });
    openEditWindow({
      title: lt('add_title'),
      submitLabel: lt('add_submit'),
      subject: lt('add_subject'),
      people: candidates,
      anchor,
      errorMessage: (err) => err.message,
      fields: [
        { key: 'name', label: lt('f_position_name'), kind: 'text', required: true, maxLength: 120 },
        { key: 'unit', label: ot('col_unit'), kind: 'select', options: units, required: true },
        { key: 'parent', label: lt('f_reports_to'), kind: 'select', options: parents, hint: lt('f_reports_to_hint') },
        { key: 'person', label: lt('f_person'), kind: 'person', hint: lt('f_person_hint') },
        { key: 'share', label: lt('f_share'), kind: 'number', value: 1, min: 0.01, max: 1, step: 0.05, hint: lt('f_share_hint') },
        { key: 'from', label: lt('f_position_from'), kind: 'date', value: day, required: true },
      ],
      async onSubmit(values) {
        const created = await orgWrite('orgPositionCreateRequest', {
          unitId: values.unit,
          name: values.name,
          parentPositionId: values.parent,
          validFrom: values.from,
        });
        const unitName = units.find((u) => u.value === values.unit)?.label ?? '';
        const message = lt('toast_position_added', { position: values.name, unit: unitName });
        const positionId = created.result?.value?.position_id;
        // Ending the position on its first day removes it together with the person just assigned to it.
        const undo = async () => {
          await orgWrite('orgPositionEndRequest', { positionId, from: values.from, confirmBackdated: created.confirmedBackdated });
          await refresh();
        };
        if (!values.person) return done(created, message, undo);
        try {
          const taken = await orgWrite('orgAssignRequest', {
            positionId,
            subject: userSubject(values.person),
            assignmentType: 'permanent',
            share: values.share ?? 1,
            validFrom: values.from,
            confirmBackdated: created.confirmedBackdated,
          });
          return done(taken, message, undo);
        } catch (err) {
          // The position exists now; the window must not stay open to create it a second time.
          return done(created, lt('toast_position_added_unassigned', { position: values.name, reason: err.message }), undo);
        }
      },
    });
  }

  return { editAssignment, moveAssignment, endAssignment, assignPerson, endPosition, addPosition };
}
