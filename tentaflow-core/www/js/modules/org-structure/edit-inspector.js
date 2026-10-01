// =============================================================================
// File: modules/org-structure/edit-inspector.js
// Description: The inspector of the edit mode (mockup F02, right column): the
//   selected position with its assignment, its unit with head and deputy heads,
//   or a unit alone. Every field writes on its own — a text on leaving the field,
//   a choice or a switch at once — through the handlers the edit mode gives it,
//   so what is drawn is always what the server holds after the last refresh.
//   Provenance of a field (who set it, from where) is not stored anywhere yet, so
//   the inspector shows none rather than invent one.
// =============================================================================

import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-avatar.js';
import '/js/components/tf-color-input.js';
import '/js/components/tf-alert.js';
import { daysBetween, formatDay } from '/js/lib/date-format.js';
import { branchIds } from '/js/modules/org-structure/edit-rules.js';
import { initialsOf } from '/js/modules/org-structure/tree.js';
import { textWidth } from '/js/modules/org-structure/text-metrics.js';

const t = (key, params) => I18n.t(`org_structure.edit.${key}`, params);
const base = (key, params) => I18n.t(`org_structure.${key}`, params);

const TYPES = ['permanent', 'acting', 'contractor'];
const NONE = '';
const HEX = /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i;

const seatLabel = (node) => (node.vacant ? `${base('vacancy')} · ${node.role}` : `${node.name} · ${node.role}`);
const positionOf = (view, id) => (view.positions ?? []).find((p) => p.position_id === id);
const unitOf = (view, id) => (view.units ?? []).find((u) => u.unit_id === id);

function option(value, label, selected) {
  return `<option value="${escapeAttr(value)}"${selected ? ' selected' : ''}>${escapeHtml(label)}</option>`;
}

// What a select can show of its current choice inside the inspector's width; a longer one is repeated in full under it.
const SELECT_TEXT_PX = 240;

function select(edit, label, options, value) {
  const current = options.find((o) => o.value === (value ?? NONE));
  const repeated = current && textWidth(current.label, 13, 500) > SELECT_TEXT_PX
    ? `<div class="org-insp-hint org-insp-selected-full">${escapeHtml(current.label)}</div>` : '';
  return `<tf-select data-edit="${edit}" label="${escapeAttr(label)}" value="${escapeAttr(value ?? NONE)}">`
    + `${options.map((o) => option(o.value, o.label, o.value === (value ?? NONE))).join('')}</tf-select>${repeated}`;
}

function input(edit, label, value, extra = '') {
  return `<tf-input data-edit="${edit}" label="${escapeAttr(label)}" value="${escapeAttr(value ?? '')}" ${extra}></tf-input>`;
}

function toggle(edit, label, checked, hint = '') {
  return `<div class="org-insp-toggle"><tf-toggle data-edit="${edit}" aria-label="${escapeAttr(label)}"${checked ? ' checked' : ''}></tf-toggle>`
    + `<span>${escapeHtml(label)}</span></div>${hint ? `<div class="org-insp-hint">${escapeHtml(hint)}</div>` : ''}`;
}

function section(title, body, id = '') {
  return `<section class="org-insp-section"${id ? ` id="${id}"` : ''}><h4 class="org-insp-title">${escapeHtml(title)}</h4>${body}</section>`;
}

function button(act, label, { variant = 'ghost', icon = '', data = '' } = {}) {
  return `<tf-button variant="${variant}" size="sm" data-act="${act}"${icon ? ` icon="${icon}"` : ''} ${data}>${escapeHtml(label)}</tf-button>`;
}

// ---- position ---------------------------------------------------------------

function positionSection(ctx) {
  const { model, view, node } = ctx;
  const position = positionOf(view, node.id);
  const blocked = branchIds(new Map(model.nodes.map((n) => [n.id, n])), node.id);
  const parents = [{ value: NONE, label: t('no_manager') }].concat(
    model.nodes.filter((n) => !blocked.has(n.id) && !n.staff).map((n) => ({ value: n.id, label: seatLabel(n) })),
  );
  const roles = [{ value: NONE, label: t('role_none') }].concat(ctx.roles.map((r) => ({ value: r.id, label: r.name })));
  return section(t('sec_position'), [
    input('position.name', t('position_name'), position.name),
    input('position.code', t('position_code'), position.code ?? '', `hint="${escapeAttr(t('position_code_hint'))}"`),
    input('position.unit', base('col_unit'), node.unitName, `readonly hint="${escapeAttr(t('position_unit_hint'))}"`),
    select('position.parent', t('reports_to'), parents, node.parentId ?? NONE),
    toggle('position.staff', t('position_staff'), Boolean(position.is_staff), t('position_staff_hint')),
    select('position.role', t('position_role'), roles, position.role_id ?? NONE),
  ].join(''));
}

function assignmentBlock(assignment) {
  const key = `${assignment.position_id}|${assignment.subject.kind}:${assignment.subject.id}`;
  const types = TYPES.map((value) => ({ value, label: t(`type_${value}`) }));
  return `<div class="org-insp-assignment" data-assignment="${escapeAttr(key)}">`
    + `<div class="org-insp-holder"><tf-avatar size="sm" initials="${escapeAttr(initialsOf(assignment.display_name))}"></tf-avatar>`
    + `<span class="org-insp-holder-name">${escapeHtml(assignment.display_name || base('unknown_person'))}</span>`
    + `${assignment.subject.kind === 'external' ? `<tf-chip variant="outline">${escapeHtml(t('external_person'))}</tf-chip>` : ''}</div>`
    + `<div class="org-insp-row">${input('assignment.share', t('assignment_share'), String(assignment.share), 'type="number" min="0.05" max="1" step="0.05" stepper')}`
    + `${select('assignment.type', t('assignment_type'), types, assignment.assignment_type)}</div>`
    + toggle('assignment.primary', t('assignment_primary'), Boolean(assignment.is_primary))
    + `<div class="org-insp-row">${input('assignment.from', t('assignment_from'), formatDay(assignment.valid_from), 'readonly')}`
    + `${input('assignment.to', t('assignment_to'), assignment.valid_to ? formatDay(assignment.valid_to) : t('open_ended'), 'readonly')}</div>`
    + `<div class="org-insp-actions">${button('assignment-replace', t('assignment_replace'), { icon: 'user', data: `data-key="${escapeAttr(key)}"` })}`
    + `${button('assignment-end', t('assignment_end'), { icon: 'user-x', data: `data-key="${escapeAttr(key)}"` })}</div></div>`;
}

function assignmentSection(ctx) {
  const holders = (ctx.view.assignments ?? []).filter((a) => a.position_id === ctx.node.id);
  const body = holders.length
    ? holders.map(assignmentBlock).join('')
      + `<div class="org-insp-actions">${button('assign', t('assignment_add'), { icon: 'plus' })}</div>`
    : `<div class="org-insp-vacant">${escapeHtml(t('vacant_note'))}</div>`
      + `<div class="org-insp-actions">${button('assign', t('assign_person'), { variant: 'primary', icon: 'user' })}</div>`;
  return section(t('sec_assignment'), body);
}

// ---- unit -------------------------------------------------------------------

function unitSection(ctx) {
  const { view, unit, model } = ctx;
  const source = unitOf(view, unit.id);
  const types = [{ value: NONE, label: t('type_none') }].concat(ctx.unitTypes.map((ty) => ({ value: ty.id, label: ty.name })));
  const blocked = branchIds(new Map(model.units.map((u) => [u.id, u])), unit.id);
  const parents = [{ value: NONE, label: t('no_parent_unit') }].concat(
    model.units.filter((u) => !blocked.has(u.id)).map((u) => ({ value: u.id, label: u.name })),
  );
  return section(t('sec_unit', { unit: unit.name }), [
    input('unit.name', t('unit_name'), source.name),
    `<div class="org-insp-row">${input('unit.code', t('unit_code'), source.code ?? '')}`
    + `<tf-color-input data-edit="unit.color" label="${escapeAttr(t('unit_color'))}" value="${escapeAttr(HEX.test(source.color ?? '') ? source.color : unit.color)}"></tf-color-input></div>`,
    select('unit.type', t('unit_type'), types, source.type_id ?? NONE),
    select('unit.parent', t('unit_parent'), parents, unit.parentId ?? NONE),
    `<div class="org-insp-actions">${button('unit-end', t('unit_end'), { icon: 'history' })}</div>`,
  ].join(''));
}

function leadershipSection(ctx) {
  const { view, unit, model } = ctx;
  const source = unitOf(view, unit.id);
  const byId = new Map(model.nodes.map((n) => [n.id, n]));
  const seat = (id) => {
    const node = byId.get(id);
    return node ? seatLabel(node) : '';
  };
  const head = source.head_position_id
    ? `<div class="org-insp-seat"><tf-avatar size="sm" initials="${escapeAttr(initialsOf(byId.get(source.head_position_id)?.name))}"></tf-avatar>`
      + `<span class="org-insp-seat-name">${escapeHtml(seat(source.head_position_id))}</span></div>`
    : `<div class="org-insp-seat org-insp-seat--empty">${escapeHtml(base('no_head'))}</div>`;
  const deputies = (source.deputy_head_position_ids ?? []).map((id, index) => (
    `<li class="org-insp-deputy" data-pos="${escapeAttr(id)}" data-index="${index}" draggable="true" tabindex="0" `
    + `aria-label="${escapeAttr(t('deputy_row', { position: index + 1, name: seat(id) }))}">`
    + `<span class="org-insp-deputy-no">${index + 1}</span>`
    + `<tf-avatar size="sm" initials="${escapeAttr(initialsOf(byId.get(id)?.name))}"></tf-avatar>`
    + `<span class="org-insp-seat-name">${escapeHtml(seat(id))}</span>`
    + '<svg class="icon org-insp-grip" aria-hidden="true"><use href="#i-grip"/></svg>'
    + `${button('deputy-remove', '', { icon: 'x', data: `data-pos="${escapeAttr(id)}" aria-label="${escapeAttr(t('deputy_remove'))}"` })}</li>`
  )).join('');
  return section(t('sec_leadership', { unit: unit.name }), [
    `<div class="org-insp-label">${escapeHtml(t('head'))}</div>`,
    `<div class="org-insp-head-row">${head}${button('head-pick', t('head_change'), { icon: 'edit' })}</div>`,
    `<div class="org-insp-label">${escapeHtml(t('deputies'))}</div>`,
    `<ol class="org-insp-deputies" aria-label="${escapeAttr(t('deputies'))}">${deputies}</ol>`,
    `<div class="org-insp-actions">${button('deputy-add', t('deputy_add'), { icon: 'plus' })}</div>`,
    `<div class="org-insp-hint">${escapeHtml(t('deputies_hint'))}${deputies ? ` ${escapeHtml(t('deputies_reorder_hint'))}` : ''}</div>`,
  ].join(''));
}

// ---- footer: the day and what the change reaches -----------------------------

function dayChip(at, today) {
  const days = daysBetween(today, at);
  if (days > 0) return `<tf-chip variant="outline" status="info">${escapeHtml(t('in_days', { count: days }))}</tf-chip>`;
  if (days < 0) return `<tf-chip variant="outline" status="warn">${escapeHtml(t('days_ago', { count: -days }))}</tf-chip>`;
  return `<tf-chip variant="outline" status="ok">${escapeHtml(t('today'))}</tf-chip>`;
}

function footerSection(ctx) {
  const impact = ctx.node ? ctx.impact : null;
  return section(t('sec_since'), `<div class="org-insp-since"><tf-chip variant="outline" icon="calendar">${escapeHtml(formatDay(ctx.at))}</tf-chip>${dayChip(ctx.at, ctx.today)}</div>`
    + (impact ? `<div class="org-insp-impact">${escapeHtml(t('impact', { count: impact.people }))}</div>` : ''));
}

/**
 * The inspector markup. `ctx`: `{ view, model, selection, roles, unitTypes, at, today, impact, errors }` — `errors` are the
 * sentences of the draft's operations the structure refused for this card —
 * `selection` is `{ id, kind: 'position' | 'unit' }`.
 */
export function inspectorHtml(ctx) {
  const { selection, model } = ctx;
  let node = null;
  let unit;
  if (selection.kind === 'unit') {
    unit = model.units.find((u) => u.id === selection.id);
  } else {
    node = model.nodes.find((n) => n.id === selection.id);
    unit = node ? model.units.find((u) => u.id === node.unitId) : null;
  }
  if (!unit && !node) return '';
  const local = { ...ctx, node, unit };
  const title = node ? (node.vacant ? `— ${base('vacancy')} —` : node.name) : unit.name;
  const parent = node?.parentId ? model.nodes.find((n) => n.id === node.parentId) : null;
  const subtitle = node
    ? [node.role, node.unitName, parent ? `→ ${parent.vacant ? base('vacancy') : parent.name}` : ''].filter(Boolean).join(' · ')
    : [unit.typeName, unit.code].filter(Boolean).join(' · ');
  const avatar = node && !node.vacant ? `<tf-avatar size="lg" initials="${escapeAttr(initialsOf(node.name))}"></tf-avatar>` : '';
  const head = `<div class="org-insp-head">${avatar}<div class="org-insp-titles"><div class="org-detail-title">${escapeHtml(title)}</div>`
    + `<div class="org-detail-sub">${escapeHtml(subtitle)}</div></div>`
    + `${button('close', '', { icon: 'x', data: `aria-label="${escapeAttr(base('detail_close'))}"` })}</div>`;
  const refused = (ctx.errors ?? []).map((text) => `<tf-alert class="org-insp-error" tone="danger" message="${escapeAttr(text)}"></tf-alert>`).join('');
  return head
    + refused
    + (node ? positionSection(local) + assignmentSection(local) : '')
    + unitSection(local)
    + leadershipSection(local)
    + footerSection(local);
}

/** The value a change event carries, whichever tf-* control raised it. */
function valueOf(event) {
  const detail = event.detail ?? {};
  return 'checked' in detail ? detail.checked : detail.value;
}

const fail = (el, message) => {
  el.setAttribute('error', message);
  return false;
};

/**
 * Wires the inspector in `host` once. `handlers` receives already-validated values:
 * `field(name, value, el)` for a field edit, `act(name, dataset, anchor)` for a button and
 * `reorder(fromIndex, toIndex)` for a deputy head moved in the list.
 */
export function bindInspector(host, handlers) {
  host.addEventListener('change', (e) => {
    const el = e.target.closest?.('[data-edit]');
    if (!el || el.hasAttribute('readonly')) return;
    const field = el.dataset.edit;
    let value = valueOf(e);
    if (field === 'assignment.share') {
      const share = Number(String(value).replace(',', '.'));
      if (!(share > 0 && share <= 1)) { fail(el, t('share_invalid')); return; }
      value = share;
    } else if (field === 'position.name' || field === 'unit.name') {
      value = String(value ?? '').trim();
      if (!value) { fail(el, t('name_required')); return; }
    } else if (typeof value === 'string' && (field === 'position.code' || field === 'unit.code')) {
      value = value.trim() || null;
    } else if (typeof value === 'string' && value === NONE) {
      value = null;
    }
    el.removeAttribute('error');
    handlers.field(field, value, el);
  });

  host.addEventListener('click', (e) => {
    const target = e.target.closest?.('[data-act]');
    if (target) handlers.act(target.dataset.act, target.dataset, target);
  });

  // Deputy heads: dragged by the mouse or moved with Alt+Up / Alt+Down from the keyboard.
  let dragged = null;
  host.addEventListener('dragstart', (e) => {
    const row = e.target.closest?.('.org-insp-deputy');
    if (!row) return;
    dragged = Number(row.dataset.index);
    e.dataTransfer?.setData('text/plain', row.dataset.pos);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  });
  host.addEventListener('dragover', (e) => {
    if (dragged !== null && e.target.closest?.('.org-insp-deputy')) e.preventDefault();
  });
  host.addEventListener('drop', (e) => {
    const row = e.target.closest?.('.org-insp-deputy');
    if (dragged === null || !row) return;
    e.preventDefault();
    const from = dragged;
    dragged = null;
    handlers.reorder(from, Number(row.dataset.index));
  });
  host.addEventListener('dragend', () => { dragged = null; });
  host.addEventListener('keydown', (e) => {
    const row = e.target.closest?.('.org-insp-deputy');
    if (!row || !e.altKey || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
    e.preventDefault();
    const from = Number(row.dataset.index);
    handlers.reorder(from, from + (e.key === 'ArrowUp' ? -1 : 1));
  });
}
