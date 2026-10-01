// =============================================================================
// File: modules/org-structure/edit-actions.js
// Description: What the administrator can DO in the edit mode, in one place: the
//   "⋯" menus of positions and units, and every window and inline change the
//   menus, the inspector and the drag and drop lead to. Each action builds
//   operations (edit-ops.js) and adds them to the draft (edit-draft.js), which
//   checks them with a dry run before it keeps them: an operation the structure
//   refuses is not added, and the refusal — typed, `OrgWriteError.code` — is
//   shown where the action was taken (in its window, or as a toast). Nothing is
//   written here; "Zapisz zmiany" is the edit mode's. The windows are the shared
//   ones of lib/actions; this module only says what they ask and what they add.
// =============================================================================

import { I18n } from '/js/i18n.js';
import { formatDay } from '/js/lib/date-format.js';
import { TfToast } from '/js/components/tf-toast.js';
import '/js/components/tf-radio.js';
import { openActionMenu, openConfirmWindow, openEditWindow, openMoveWindow } from '/js/lib/actions/index.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { subjectKey } from '/js/modules/org-structure/model.js';
import { OrgWriteError } from '/js/modules/org-structure/edit-draft.js';
import {
  branchIds, cardDropRule, moveTargets, positionMoveCheck, reparentImpact, reorder, unitMoveCheck, unitMoveTargets, unitSeatCandidates,
} from '/js/modules/org-structure/edit-rules.js';
import {
  assignOps, assignmentRef, createPositionOp, createUnitOp, endAssignmentOp, endPositionOp, endUnitOp, movePositionOp,
  moveUnitOp, setDeputiesOp, setHeadOp, updateAssignmentOp, updatePositionOp, updateUnitOp,
} from '/js/modules/org-structure/edit-ops.js';
import { TEMPLATES, templateCounts, templateOps, templateOutline } from '/js/modules/org-structure/edit-templates.js';
import { openAssignPersonWindow } from '/js/modules/org-structure/edit-assign-window.js';
import { openHandover } from '/js/modules/org-structure/handover-nav.js';
import { escapeHtml } from '/js/utils.js';

const t = (key, params) => I18n.t(`org_structure.edit.${key}`, params);
const base = (key, params) => I18n.t(`org_structure.${key}`, params);

const NO_TARGET = '__none';
const REASONS = ['unknown', 'self', 'cycle', 'staff', 'same'];

/** Text for a refusal: the typed code has a sentence of its own, the server's message is the fallback. */
export function errorText(err) {
  const code = err?.code;
  if (code) {
    const key = `err_${code}`;
    const text = t(key);
    if (text !== `org_structure.edit.${key}`) return text;
  }
  return err?.message || t('err_internal');
}

const reasonTexts = () => Object.fromEntries(REASONS.map((r) => [r, t(`reason_${r}`)]));
const seatLabel = (node) => (node.vacant ? `${base('vacancy')} · ${node.role}` : `${node.name} · ${node.role}`);
const yesNo = (value) => ({ value, label: t(value === 'yes' ? 'yes' : 'no') });

/** Holder names of a position, or its own name while nobody holds it: how a card is named in a sentence. */
export function seatName(view, positionId) {
  const holders = (view.assignments ?? []).filter((a) => a.position_id === positionId && a.display_name);
  if (holders.length) return holders.map((a) => a.display_name).join(', ');
  return (view.positions ?? []).find((p) => p.position_id === positionId)?.name ?? '';
}

/**
 * @param {object} env
 * @param {object} env.draft edit-draft.js
 * @param {() => object} env.getView the structure the canvas shows (the draft's preview)
 * @param {() => object} env.getModel tree model of that view
 * @param {() => string} env.getAt the effective day
 * @param {() => Array} env.people accounts `{ id, name, email }`
 * @param {() => Array} env.roles `{ id, name }`
 * @param {() => Array} env.unitTypes `{ id, name }`
 * @param {(id: string, kind: string) => void} env.select selects a card
 */
export function createActions(env) {
  const { draft } = env;

  // ---- adding to the draft -----------------------------------------------------

  function askBackdated(date) {
    return new Promise((resolve) => {
      let confirmed = false;
      const win = openConfirmWindow({
        kind: 'archive',
        title: t('backdated_title'),
        submitLabel: t('backdated_submit'),
        consequence: t('backdated_consequence', { date: formatDay(date) }),
        onSubmit: async () => { confirmed = true; },
      });
      win.addEventListener('closed', () => resolve(confirmed));
    });
  }

  /** Adds operations to the draft (one undo step); throws the refusal. `after(created)` runs with what they created. */
  async function perform(ops, after = null) {
    if (!ops) return false;
    const created = await draft.addChecked(Array.isArray(ops) ? ops : [ops]);
    after?.(created);
    return true;
  }

  function failure(err) {
    TfToast.show({ tone: 'danger', message: errorText(err), duration: 7000 });
  }

  /** The same, outside a window (a drop, a field of the inspector): a refusal is a toast. */
  async function inline(ops, after = null) {
    try {
      return await perform(ops, after);
    } catch (err) {
      failure(err);
      return false;
    }
  }

  /** Selects the first thing of `kind` the operations created, by the id the preview shows it under. */
  const selectCreated = (kind, selectKind) => (created) => {
    const made = created.find((op) => op.kind === kind);
    if (made) env.select(made.tempId, selectKind);
  };

  const view = () => env.getView();
  const model = () => env.getModel();
  const at = () => env.getAt();
  const nodeOf = (id) => model().nodes.find((n) => n.id === id);
  const unitModelOf = (id) => model().units.find((u) => u.id === id);

  // ---- moving under another manager / unit -----------------------------------

  function reparentNote(id, targetName) {
    const impact = reparentImpact(model(), id);
    return { tone: 'warning', text: t('reparent_consequence', { count: impact.people, target: targetName }) };
  }

  /** A card dropped on another one (or "Raportuje do" changed): says why not, or asks and moves. */
  function reparentPosition(sourceId, targetId, anchor = null) {
    const check = positionMoveCheck(model(), sourceId, targetId);
    if (!check.ok) {
      TfToast.show({ tone: 'danger', message: t(`drop_${check.reason}`), duration: 6000 });
      return null;
    }
    const target = targetId ? seatName(view(), targetId) : t('no_manager');
    return openConfirmWindow({
      kind: 'archive',
      title: t('reparent_title'),
      submitLabel: t('reparent_submit'),
      subject: t('reparent_subject', { who: seatName(view(), sourceId), target }),
      consequence: reparentNote(sourceId, target).text,
      anchor,
      errorMessage: errorText,
      onSubmit: () => perform(movePositionOp(sourceId, targetId, at())),
    });
  }

  function reparentUnit(sourceId, targetId, anchor = null) {
    const check = unitMoveCheck(model(), sourceId, targetId);
    if (!check.ok) {
      TfToast.show({ tone: 'danger', message: t(`drop_unit_${check.reason}`), duration: 6000 });
      return null;
    }
    const unit = unitModelOf(sourceId);
    const target = targetId ? unitModelOf(targetId).name : t('no_parent_unit');
    const people = new Set();
    const units = new Map(model().units.map((u) => [u.id, u]));
    const inBranch = branchIds(units, sourceId);
    for (const node of model().nodes) if (inBranch.has(node.unitId)) node.people.forEach((p) => people.add(p.key));
    return openConfirmWindow({
      kind: 'archive',
      title: t('move_unit_title'),
      submitLabel: t('move_unit_submit'),
      subject: t('reparent_subject', { who: unit.name, target }),
      consequence: t('move_unit_consequence', { count: people.size, target }),
      anchor,
      errorMessage: errorText,
      onSubmit: () => perform(moveUnitOp(sourceId, targetId, at())),
    });
  }

  function movePositionWindow(id, anchor) {
    const node = nodeOf(id);
    const targets = [{ id: NO_TARGET, label: t('no_manager'), ...(node.parentId ? {} : { disabled: t('reason_same') }) }]
      .concat(moveTargets(model(), id, { describe: seatLabel, reasons: reasonTexts() }));
    return openMoveWindow({
      subject: seatLabel(node),
      title: t('move_position_title'),
      submitLabel: t('move_position_submit'),
      targets,
      selected: node.parentId ?? NO_TARGET,
      noteFor: (target) => reparentNote(id, target.id === NO_TARGET ? t('no_manager') : seatName(view(), target.id)),
      anchor,
      errorMessage: errorText,
      onSubmit: ({ targetId }) => {
        const target = targetId === NO_TARGET ? null : targetId;
        const check = positionMoveCheck(model(), id, target);
        if (!check.ok) throw new OrgWriteError({ code: `drop_${check.reason}`, message: t(`drop_${check.reason}`) });
        return perform(movePositionOp(id, target, at()));
      },
    });
  }

  function moveUnitWindow(id, anchor) {
    const unit = unitModelOf(id);
    const targets = [{ id: NO_TARGET, label: t('no_parent_unit'), ...(unit.parentId ? {} : { disabled: t('reason_same') }) }]
      .concat(unitMoveTargets(model(), id, { reasons: reasonTexts() }));
    return openMoveWindow({
      subject: unit.name,
      title: t('move_unit_title'),
      submitLabel: t('move_unit_submit'),
      targets,
      selected: unit.parentId ?? NO_TARGET,
      anchor,
      errorMessage: errorText,
      onSubmit: ({ targetId }) => {
        const target = targetId === NO_TARGET ? null : targetId;
        const check = unitMoveCheck(model(), id, target);
        if (!check.ok) throw new OrgWriteError({ code: `drop_unit_${check.reason}`, message: t(`drop_unit_${check.reason}`) });
        return perform(moveUnitOp(id, target, at()));
      },
    });
  }

  // ---- people ----------------------------------------------------------------

  function externalsOf() {
    const seen = new Map();
    for (const a of view().assignments ?? []) {
      if (a.subject.kind === 'external' && !seen.has(a.subject.id)) seen.set(a.subject.id, { id: a.subject.id, name: a.display_name });
    }
    return [...seen.values()];
  }

  function assignmentOf(key) {
    const [positionId, subject] = key.split('|');
    return (view().assignments ?? []).find((a) => a.position_id === positionId && subjectKey(a.subject) === subject) ?? null;
  }

  function assignWindow(positionId, anchor, previous = null) {
    const node = nodeOf(positionId);
    return openAssignPersonWindow({
      subject: seatLabel(node),
      title: previous ? t('replace_title') : t('assign_title'),
      submitLabel: previous ? t('replace_submit') : t('assign_submit'),
      note: previous ? { tone: 'info', text: t('replace_note', { name: previous.display_name, date: formatDay(at()) }) } : null,
      people: env.people(),
      externals: externalsOf(),
      anchor,
      errorMessage: errorText,
      onSubmit: (values) => perform(
        assignOps(positionId, values.person, values, at(), { endPrevious: previous }),
        () => env.select(positionId, 'position'),
      ),
    });
  }

  /** A person dragged from the strip onto a vacancy: assigned at once with the defaults, refined in the inspector. */
  function assignDropped(userId, positionId) {
    const person = env.people().find((p) => p.id === userId);
    if (!person) return Promise.resolve(false);
    return inline(
      assignOps(positionId, { kind: 'user', id: person.id }, { assignmentType: 'permanent', share: 1, isPrimary: null }, at()),
      () => env.select(positionId, 'position'),
    );
  }

  // The keyboard's way to what a drag does: pick the person, then the vacancy they take.
  function pickVacancyWindow(userId, anchor) {
    const person = env.people().find((p) => p.id === userId);
    const targets = model().nodes.filter((n) => n.vacant).map((n) => ({ id: n.id, label: `${n.role} · ${n.unitName}` }));
    if (!person) return null;
    if (!targets.length) {
      TfToast.show({ tone: 'warning', message: t('no_vacancies'), duration: 6000 });
      return null;
    }
    return openMoveWindow({
      subject: person.name,
      title: t('pick_vacancy_title'),
      submitLabel: t('assign_submit'),
      targets,
      anchor,
      errorMessage: errorText,
      onSubmit: ({ targetId }) => perform(
        assignOps(targetId, { kind: 'user', id: person.id }, { assignmentType: 'permanent', share: 1, isPrimary: null }, at()),
        () => env.select(targetId, 'position'),
      ),
    });
  }

  function endAssignmentWindow(assignment, anchor) {
    const position = nodeOf(assignment.position_id);
    return openConfirmWindow({
      kind: 'delete',
      title: t('end_assignment_title'),
      submitLabel: t('end_assignment_submit'),
      subject: `${assignment.display_name} · ${position.role}`,
      consequence: t('end_assignment_consequence', { who: assignment.display_name, position: position.role, date: formatDay(at()) }),
      anchor,
      errorMessage: errorText,
      onSubmit: () => perform(endAssignmentOp(assignment, at())),
    });
  }

  // ---- fields ----------------------------------------------------------------

  function roleOptions() {
    return env.roles().map((r) => ({ value: r.id, label: r.name }));
  }

  function editPositionWindow(id, anchor) {
    const position = view().positions.find((p) => p.position_id === id);
    return openEditWindow({
      subject: seatLabel(nodeOf(id)),
      title: t('edit_position_title'),
      submitLabel: t('add_to_draft'),
      fields: [
        { key: 'name', label: t('position_name'), kind: 'text', required: true, value: position.name },
        { key: 'code', label: t('position_code'), kind: 'text', value: position.code ?? '', hint: t('position_code_hint') },
        { key: 'staff', label: t('position_staff'), kind: 'select', required: true, value: position.is_staff ? 'yes' : 'no', options: [yesNo('yes'), yesNo('no')], hint: t('position_staff_hint') },
        { key: 'role', label: t('position_role'), kind: 'select', value: position.role_id ?? '', options: roleOptions() },
      ],
      anchor,
      errorMessage: errorText,
      onSubmit: (values, { changed }) => {
        const patch = {};
        if (changed.includes('name')) patch.name = values.name;
        if (changed.includes('code')) patch.code = values.code || null;
        if (changed.includes('staff')) patch.isStaff = values.staff === 'yes';
        if (changed.includes('role')) patch.roleId = values.role;
        return perform(updatePositionOp(view(), id, patch, at()));
      },
    });
  }

  function editUnitWindow(id, anchor) {
    const unit = view().units.find((u) => u.unit_id === id);
    return openEditWindow({
      subject: unit.name,
      title: t('edit_unit_title'),
      submitLabel: t('add_to_draft'),
      fields: [
        { key: 'name', label: t('unit_name'), kind: 'text', required: true, value: unit.name },
        { key: 'code', label: t('unit_code'), kind: 'text', value: unit.code ?? '' },
        { key: 'type', label: t('unit_type'), kind: 'select', value: unit.type_id ?? '', options: env.unitTypes().map((ty) => ({ value: ty.id, label: ty.name })) },
      ],
      anchor,
      errorMessage: errorText,
      onSubmit: (values, { changed }) => {
        const patch = {};
        if (changed.includes('name')) patch.name = values.name;
        if (changed.includes('code')) patch.code = values.code || null;
        if (changed.includes('type')) patch.typeId = values.type;
        return perform(updateUnitOp(view(), id, patch, at()));
      },
    });
  }

  function addUnitWindow(parentUnitId = null, anchor = null) {
    const units = model().units.map((u) => ({ value: u.id, label: u.name }));
    return openEditWindow({
      subject: null,
      title: t('add_unit_title'),
      submitLabel: t('add_unit_submit'),
      note: { tone: 'info', text: t('created_from', { date: formatDay(at()) }) },
      fields: [
        { key: 'name', label: t('unit_name'), kind: 'text', required: true },
        { key: 'code', label: t('unit_code'), kind: 'text' },
        { key: 'type', label: t('unit_type'), kind: 'select', options: env.unitTypes().map((ty) => ({ value: ty.id, label: ty.name })) },
        { key: 'parent', label: t('unit_parent'), kind: 'select', value: parentUnitId ?? '', options: units },
      ],
      anchor,
      errorMessage: errorText,
      onSubmit: (values) => perform(
        createUnitOp({ name: values.name, code: values.code || null, typeId: values.type, parentUnitId: values.parent }, at()),
        selectCreated('unitCreate', 'unit'),
      ),
    });
  }

  function addPositionWindow({ unitId = null, parentPositionId = null } = {}, anchor = null) {
    if (!model().units.length) {
      TfToast.show({ tone: 'warning', message: t('add_position_needs_unit'), duration: 6000 });
      return null;
    }
    const units = model().units.map((u) => ({ value: u.id, label: u.name }));
    const parents = model().nodes.filter((n) => !n.staff).map((n) => ({ value: n.id, label: seatLabel(n) }));
    return openEditWindow({
      subject: null,
      title: t('add_position_title'),
      submitLabel: t('add_position_submit'),
      note: { tone: 'info', text: t('created_from', { date: formatDay(at()) }) },
      fields: [
        { key: 'name', label: t('position_name'), kind: 'text', required: true },
        { key: 'code', label: t('position_code'), kind: 'text', hint: t('position_code_hint') },
        { key: 'unit', label: base('col_unit'), kind: 'select', required: true, value: unitId ?? '', options: units },
        { key: 'parent', label: t('reports_to'), kind: 'select', value: parentPositionId ?? '', options: parents },
        { key: 'staff', label: t('position_staff'), kind: 'select', required: true, value: 'no', options: [yesNo('yes'), yesNo('no')] },
        { key: 'role', label: t('position_role'), kind: 'select', options: roleOptions() },
      ],
      anchor,
      errorMessage: errorText,
      onSubmit: (values) => perform(
        createPositionOp({
          unitId: values.unit, name: values.name, code: values.code || null, roleId: values.role,
          isStaff: values.staff === 'yes', parentPositionId: values.parent,
        }, at()),
        selectCreated('positionCreate', 'position'),
      ),
    });
  }

  // ---- unit leadership ---------------------------------------------------------

  // A unit with the ids of its deputy heads, which the tree model does not carry.
  function unitInfo(unitId) {
    const unit = unitModelOf(unitId);
    const source = view().units.find((u) => u.unit_id === unitId);
    return { ...unit, deputyIds: source.deputy_head_position_ids ?? [] };
  }

  function headTargets(unit) {
    return model().nodes.filter((n) => n.unitId === unit.id).map((n) => {
      const target = { id: n.id, label: seatLabel(n) };
      if (unit.deputyIds.includes(n.id)) target.disabled = t('reason_is_deputy');
      return target;
    });
  }

  function headWindow(unitId, anchor) {
    const unit = unitInfo(unitId);
    const targets = [{ id: NO_TARGET, label: t('no_head_option'), ...(unit.headId ? {} : { disabled: t('reason_same') }) }]
      .concat(headTargets(unit));
    return openMoveWindow({
      subject: unit.name,
      title: t('head_title'),
      submitLabel: t('head_submit'),
      targets,
      selected: unit.headId ?? NO_TARGET,
      note: { tone: 'info', text: t('head_note') },
      anchor,
      errorMessage: errorText,
      onSubmit: ({ targetId }) => {
        const head = targetId === NO_TARGET ? null : targetId;
        return perform(setHeadOp(unitId, head, at()));
      },
    });
  }

  function setHeadTo(unitId, positionId) {
    return inline(setHeadOp(unitId, positionId, at()));
  }

  function deputiesOf(unitId) {
    return [...(view().units.find((u) => u.unit_id === unitId)?.deputy_head_position_ids ?? [])];
  }

  function addDeputyWindow(unitId, anchor) {
    const unit = unitInfo(unitId);
    const eligible = new Set(unitSeatCandidates(view(), unitId).map((p) => p.position_id));
    const targets = model().nodes.filter((n) => n.unitId === unitId).map((n) => {
      const target = { id: n.id, label: seatLabel(n) };
      if (!eligible.has(n.id)) target.disabled = n.id === unit.headId ? t('reason_is_head') : t('reason_is_deputy');
      return target;
    });
    return openMoveWindow({
      subject: unit.name,
      title: t('deputy_add_title'),
      submitLabel: t('deputy_add_submit'),
      targets,
      note: { tone: 'info', text: t('deputies_hint') },
      anchor,
      errorMessage: errorText,
      onSubmit: ({ targetId }) => perform(setDeputiesOp(unitId, [...deputiesOf(unitId), targetId], at())),
    });
  }

  function addDeputyTo(unitId, positionId) {
    return inline(setDeputiesOp(unitId, [...deputiesOf(unitId), positionId], at()));
  }

  function removeDeputy(unitId, positionId) {
    const list = deputiesOf(unitId).filter((id) => id !== positionId);
    return inline(setDeputiesOp(unitId, list, at()));
  }

  function reorderDeputies(unitId, from, to) {
    const list = deputiesOf(unitId);
    const next = reorder(list, from, to);
    if (next.every((id, i) => id === list[i])) return Promise.resolve(false);
    return inline(setDeputiesOp(unitId, next, at()));
  }

  // ---- ending things -----------------------------------------------------------

  function endPositionWindow(id, anchor) {
    const node = nodeOf(id);
    return openConfirmWindow({
      kind: 'delete',
      title: t('end_position_title'),
      submitLabel: t('end_position_submit'),
      subject: seatLabel(node),
      consequence: t('end_position_consequence', { position: node.role, count: node.childIds.length, date: formatDay(at()) }),
      anchor,
      errorMessage: errorText,
      onSubmit: () => perform(endPositionOp(id, at())),
    });
  }

  function endUnitWindow(id, anchor) {
    const unit = unitModelOf(id);
    return openConfirmWindow({
      kind: 'delete',
      title: t('end_unit_title'),
      submitLabel: t('end_unit_submit'),
      subject: unit.name,
      consequence: t('end_unit_consequence', { unit: unit.name, positions: unit.memberIds.length, units: unit.childIds.length, date: formatDay(at()) }),
      anchor,
      errorMessage: errorText,
      onSubmit: () => perform(endUnitOp(id, at())),
    });
  }

  // ---- inline edits from the inspector ---------------------------------------

  function positionField(id, field, value) {
    const patch = { 'position.name': { name: value }, 'position.code': { code: value }, 'position.staff': { isStaff: value }, 'position.role': { roleId: value } }[field];
    return inline(updatePositionOp(view(), id, patch, at()));
  }

  function unitField(id, field, value) {
    const patch = { 'unit.name': { name: value }, 'unit.code': { code: value }, 'unit.type': { typeId: value }, 'unit.color': { color: value } }[field];
    return inline(updateUnitOp(view(), id, patch, at()));
  }

  function assignmentField(key, field, value) {
    const assignment = assignmentOf(key);
    if (!assignment) return Promise.resolve(false);
    const patch = { 'assignment.share': { share: value }, 'assignment.type': { assignmentType: value }, 'assignment.primary': { isPrimary: value } }[field];
    return inline(updateAssignmentOp(view(), assignmentRef(assignment), patch, at()));
  }

  // ---- templates ---------------------------------------------------------------

  function templateWindow(anchor) {
    const group = document.createElement('tf-radio-group');
    group.setAttribute('name', 'org-template');
    group.setAttribute('label', t('template_pick'));
    for (const template of TEMPLATES) {
      const counts = templateCounts(template);
      const radio = document.createElement('tf-radio');
      radio.setAttribute('value', template.id);
      radio.setAttribute('label', t(`template_${template.id}`));
      radio.setAttribute('hint', t('template_counts', counts));
      group.appendChild(radio);
    }
    group.setAttribute('value', TEMPLATES[0].id);
    const preview = document.createElement('div');
    preview.className = 'org-tpl-preview';
    const chosen = () => TEMPLATES.find((tpl) => tpl.id === group.value) ?? TEMPLATES[0];
    const drawPreview = () => {
      preview.innerHTML = `<div class="org-insp-label">${escapeHtml(t('template_preview'))}</div><ul class="org-tpl-outline">${
        templateOutline(chosen(), t).map((row) => `<li style="--depth:${row.depth}"><b>${escapeHtml(row.unit)}</b> <span>${escapeHtml(row.positions.join(' · '))}</span></li>`).join('')
      }</ul>`;
    };
    drawPreview();
    group.addEventListener('change', drawPreview);
    return openFormWindow({
      title: t('template_title'),
      icon: 'sparkle',
      subject: null,
      note: { tone: 'info', text: t('template_note') },
      sections: [group, preview],
      width: 640,
      submitLabel: t('template_submit'),
      anchor,
      errorMessage: errorText,
      collect: () => ({ template: chosen() }),
      onSubmit: ({ template }) => perform(templateOps(template, t, at())),
    });
  }

  // ---- menus ---------------------------------------------------------------------

  function positionMenu(node, anchor) {
    const assignments = (view().assignments ?? []).filter((a) => a.position_id === node.id);
    const unitId = node.unitId;
    const unit = unitInfo(unitId);
    const isHead = unit.headId === node.id;
    const isDeputy = unit.deputyIds.includes(node.id);
    const items = [
      { label: t('menu_edit'), icon: 'edit', run: () => editPositionWindow(node.id, anchor) },
      node.vacant
        ? { label: t('menu_assign'), icon: 'user', run: () => assignWindow(node.id, anchor) }
        : { label: t('menu_replace'), icon: 'user', run: () => assignWindow(node.id, anchor, assignments[0]) },
      { label: t('menu_move'), icon: 'arrow', run: () => movePositionWindow(node.id, anchor) },
      { separator: true },
      isHead
        ? { label: t('menu_unset_head'), icon: 'crown', run: () => inline(setHeadOp(unitId, null, at())) }
        : { label: t('menu_set_head'), icon: 'crown', disabled: isDeputy, reason: isDeputy ? t('reason_is_deputy') : undefined, run: () => setHeadTo(unitId, node.id) },
      { label: t('menu_add_deputy'), icon: 'plus', disabled: isHead || isDeputy, reason: isHead ? t('reason_is_head') : isDeputy ? t('reason_is_deputy') : undefined, run: () => addDeputyTo(unitId, node.id) },
      { separator: true },
    ];
    // Work is held by an account: the handover screen exists for people who have one.
    const holder = assignments[0]?.subject;
    if (!node.vacant && holder?.kind === 'user') {
      items.push({ label: I18n.t('org_structure.handover.menu'), icon: 'send', danger: true, run: () => openHandover({ userId: holder.id, reason: 'departure' }) });
    }
    if (!node.vacant) items.push({ label: t('menu_end_assignment'), icon: 'user', danger: true, run: () => endAssignmentWindow(assignments[0], anchor) });
    items.push({ label: t('menu_end_position'), icon: 'trash', danger: true, run: () => endPositionWindow(node.id, anchor) });
    return openActionMenu(anchor, items, seatLabel(node));
  }

  function unitMenu(unit, anchor) {
    return openActionMenu(anchor, [
      { label: t('menu_edit_unit'), icon: 'edit', run: () => editUnitWindow(unit.id, anchor) },
      { label: t('menu_add_position'), icon: 'plus', run: () => addPositionWindow({ unitId: unit.id }, anchor) },
      { label: t('menu_add_subunit'), icon: 'layers', run: () => addUnitWindow(unit.id, anchor) },
      { label: t('menu_pick_head'), icon: 'crown', run: () => headWindow(unit.id, anchor) },
      { label: t('menu_move_unit'), icon: 'arrow', run: () => moveUnitWindow(unit.id, anchor) },
      { separator: true },
      { label: t('menu_end_unit'), icon: 'trash', danger: true, run: () => endUnitWindow(unit.id, anchor) },
    ], unit.name);
  }

  return {
    errorText,
    askBackdated,
    dropRule: () => cardDropRule(model()),
    reparentPosition,
    reparentUnit,
    assignWindow,
    assignDropped,
    pickVacancyWindow,
    endAssignmentWindow,
    assignmentOf,
    addUnitWindow,
    addPositionWindow,
    headWindow,
    addDeputyWindow,
    removeDeputy,
    reorderDeputies,
    endUnitWindow,
    positionField,
    unitField,
    assignmentField,
    templateWindow,
    positionMenu,
    unitMenu,
  };
}
