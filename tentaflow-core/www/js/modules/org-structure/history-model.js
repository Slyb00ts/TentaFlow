// =============================================================================
// File: modules/org-structure/history-model.js
// Description: Pure read-model of the Historia tab (mockup F04): the days on
//   the date jumper, the timeline of changes with their before/after, the
//   differences between two states of the structure (and what they mark on a
//   chart), and the state of a planned reorganization (who may approve it, what
//   its card offers). No DOM and no transport, so the rules are tested without a
//   page. Wire fields are read in their snake_case spelling, as elsewhere in the
//   module; `t` is the translator of `org_structure.history.*`.
// =============================================================================

import { addDays, daysBetween, formatDay, isIsoDay } from '/js/lib/date-format.js';

/** The day a written-at stamp (`YYYY-MM-DD HH:MM:SS`, UTC) falls on. */
export const dayOfStamp = (stamp) => String(stamp ?? '').slice(0, 10);

/** How far `dayIso` is from today: `{ kind: 'today' | 'future' | 'past', days }`, days always positive. */
export function relativeDay(dayIso, todayIso) {
  const days = daysBetween(todayIso, dayIso);
  if (days === 0) return { kind: 'today', days: 0 };
  return { kind: days > 0 ? 'future' : 'past', days: Math.abs(days) };
}

// ---------------------------------------------------------------------------
// The date jumper
// ---------------------------------------------------------------------------

/** The distinct days something changed (or is planned to), oldest first. */
export function markers(entries, changeSets, today) {
  const days = new Map();
  for (const entry of entries ?? []) {
    if (isIsoDay(entry.effective_date)) days.set(entry.effective_date, entry.effective_date > today);
  }
  for (const set of changeSets ?? []) {
    if (['draft', 'pending'].includes(set.state) && isIsoDay(set.effective_date)) days.set(set.effective_date, true);
  }
  return [...days.entries()]
    .map(([day, planned]) => ({ day, planned }))
    .sort((a, b) => a.day.localeCompare(b.day));
}

/** The slider's span: the first change (a month before today at most) to the last planned one (a month after at least). */
export function sliderRange(marks, today) {
  const days = marks.map((m) => m.day).sort();
  const first = days.length ? days[0] : today;
  const last = days.length ? days[days.length - 1] : today;
  return {
    from: first < addDays(today, -30) ? first : addDays(today, -30),
    to: last > addDays(today, 30) ? last : addDays(today, 30),
  };
}

export const sliderValueOf = (day, range) => daysBetween(range.from, day);
export const sliderDayOf = (value, range) => addDays(range.from, Number(value));

const TICK_GAP = 9;

/**
 * The labelled ticks of the slider's rail: the first day in full, today, the last day and the marked days, as many as
 * fit (`TICK_GAP` percent apart — today and the ends win over a mark). `{ day, pos, kind: 'start'|'today'|'mark' }`, left to right.
 */
export function railTicks(marks, today, range) {
  const candidates = [
    { day: today, kind: 'today' },
    { day: range.from, kind: 'start' },
    { day: range.to, kind: 'mark' },
    ...marks.map((m) => ({ day: m.day, kind: 'mark' })),
  ].map((c) => ({ ...c, pos: railPercent(c.day, range) }));
  const kept = [];
  for (const c of candidates) {
    if (kept.some((k) => k.day === c.day || Math.abs(k.pos - c.pos) < TICK_GAP)) continue;
    kept.push(c);
  }
  return kept.sort((a, b) => a.pos - b.pos);
}

/** Position of a day on the rail, 0..100. */
export function railPercent(day, range) {
  const span = daysBetween(range.from, range.to);
  return span <= 0 ? 0 : Math.min(100, Math.max(0, (daysBetween(range.from, day) / span) * 100));
}

/**
 * The days offered as "skocz do": the first change, the last one before today, today, and the planned ones
 * (at most three), in date order and without repeats.
 */
export function quickJumps(marks, today) {
  const past = marks.filter((m) => m.day < today);
  const planned = marks.filter((m) => m.day > today);
  const picks = [past[0]?.day, past.length > 1 ? past[past.length - 1].day : null, today, ...planned.slice(0, 3).map((m) => m.day)];
  return [...new Set(picks.filter(Boolean))].map((day) => ({
    day,
    kind: day === today ? 'today' : day > today ? 'planned' : 'past',
  }));
}

// ---------------------------------------------------------------------------
// The timeline
// ---------------------------------------------------------------------------

const FLAG_FIELDS = new Set(['is_manager', 'is_staff', 'is_primary']);

/** A stored value as text: the label of a reference, "brak" for nothing, words for a flag. */
export function valueText(field, value, label, t) {
  if (value === null || value === undefined || value === '') return t('none');
  if (label) return label;
  if (FLAG_FIELDS.has(field)) return value === 'true' ? t('yes') : t('no');
  if (field === 'assignment_type' || field === 'type') return t(`assignment_type.${value}`);
  if (field === 'share') {
    const share = Number(value);
    return Number.isFinite(share) ? `${Math.round(share * 100)}%` : String(value);
  }
  if (field === 'line_kind') return t(`line_kind.${value}`);
  return String(value);
}

const actionKey = (action) => String(action ?? '').replace(/^org\./, '').replaceAll('.', '_');

const DOTS = {
  unit_create: 'good', position_create: 'good', assignment_create: 'good', unit_type_create: 'good',
  unit_end: 'bad', position_end: 'bad', assignment_end: 'bad', assignment_end_for_user: 'bad', unit_type_delete: 'bad',
};

function fieldChanges(entry, t) {
  return (entry.changes ?? []).map((change) => ({
    field: change.field,
    label: t(`field.${change.field}`),
    before: valueText(change.field, change.before, change.before_label, t),
    after: valueText(change.field, change.after, change.after_label, t),
    // A creation states facts: there is nothing before it to show.
    created: String(entry.action ?? '').endsWith('.create'),
  }));
}

function opLines(entry, t, limit = 8) {
  const ops = entry.ops ?? [];
  const lines = ops.slice(0, limit).map((op) => t(`op.${actionKey(op.action)}`, { name: op.target_name || '' }));
  if (ops.length > limit) lines.push(t('ops_more', { count: ops.length - limit }));
  return lines;
}

/** One audit entry as the timeline draws it. */
export function entryModel(entry, { t, today }) {
  const key = actionKey(entry.action);
  const day = isIsoDay(entry.effective_date) ? entry.effective_date : dayOfStamp(entry.at);
  const planned = day > today;
  const title = t(`action.${key}`, {
    name: entry.target_name || '',
    unit: entry.unit_name || '',
    person: entry.subject_name || '',
    position: entry.position_name || '',
    count: (entry.ops ?? []).length,
  });
  return {
    id: entry.id,
    day,
    dayText: formatDay(day),
    planned,
    dot: planned ? 'plan' : (DOTS[key] ?? ''),
    title,
    changes: fieldChanges(entry, t),
    ops: opLines(entry, t),
    hiddenOps: entry.hidden_ops ?? 0,
    unitId: entry.unit_id ?? null,
    who: [entry.actor_name, entry.source ? t(`source.${entry.source}`) : null].filter(Boolean).join(' · '),
    writtenAt: dayOfStamp(entry.at),
  };
}

// ---------------------------------------------------------------------------
// Differences between two states
// ---------------------------------------------------------------------------

/** The counts of a diff: what was added, removed and changed, and in which kind of thing. */
export function diffCounts(items) {
  const counts = { added: 0, removed: 0, changed: 0, units: 0, positions: 0, assignments: 0 };
  const seen = new Set();
  for (const item of items ?? []) {
    counts[item.change] = (counts[item.change] ?? 0) + 1;
    // A thing with three changed fields is one thing.
    const key = `${item.entity}:${item.id}:${item.subject?.id ?? ''}`;
    if (!seen.has(key)) {
      seen.add(key);
      counts[`${item.entity}s`] += 1;
    }
  }
  return counts;
}

/** One difference as a sentence and (for a changed field) the value before and after. */
export function diffLine(item, t) {
  const person = item.subject_name || t('diff.someone');
  if (item.entity === 'assignment') {
    if (item.change === 'added') return { text: t('diff.assignment_added', { person, name: item.name }) };
    if (item.change === 'removed') return { text: t('diff.assignment_removed', { person, name: item.name }) };
    return {
      text: t('diff.assignment_changed', { person, name: item.name, field: t(`field.${item.field}`) }),
      before: valueText(item.field, item.before, item.before_label, t),
      after: valueText(item.field, item.after, item.after_label, t),
    };
  }
  if (item.change === 'added') return { text: t(`diff.${item.entity}_added`, { name: item.name }) };
  if (item.change === 'removed') return { text: t(`diff.${item.entity}_removed`, { name: item.name }) };
  return {
    text: t(`diff.${item.entity}_changed`, { name: item.name, field: t(`field.${item.field}`) }),
    before: valueText(item.field, item.before, item.before_label, t),
    after: valueText(item.field, item.after, item.after_label, t),
  };
}

const RANK = { changed: 1, added: 2, removed: 3 };

/**
 * What the chart marks for a diff, by strongest first: `added` (new), `changed`, and `removed` (only on the
 * "before" side). A position is marked for its own changes and for a change of who holds it.
 */
export function diffMarks(items) {
  const positions = new Map();
  const units = new Map();
  const raise = (map, id, change) => {
    if (!id) return;
    const rank = (m) => RANK[m] ?? 0;
    if (rank(map.get(id)) < rank(change)) map.set(id, change);
  };
  for (const item of items ?? []) {
    if (item.entity === 'unit') raise(units, item.id, item.change);
    else if (item.entity === 'position') raise(positions, item.id, item.change);
    else raise(positions, item.id, 'changed');
    // A unit is touched by everything in it.
    if (item.entity !== 'unit' && item.unit_id) raise(units, item.unit_id, 'changed');
  }
  return { positions, units };
}

/** The chart's own words for the marks (`ot-mark-*`): removed reads as an error on the "before" side. */
export function chartMark(mark, side) {
  if (mark === 'added') return side === 'after' ? 'added' : null;
  if (mark === 'removed') return side === 'before' ? 'error' : null;
  return mark === 'changed' ? 'changed' : null;
}

/** Writes the marks of a diff onto a tree model (`buildTreeModel` output) for the `side` it draws. */
export function decorateModel(model, marks, side) {
  for (const node of model.nodes) node.mark = chartMark(marks.positions.get(node.id), side);
  for (const unit of model.units) unit.mark = chartMark(marks.units.get(unit.id), side);
  return model;
}

// ---------------------------------------------------------------------------
// A unit's part of a structure
// ---------------------------------------------------------------------------

/** The unit and its sub-units, by the parent links of `view`. */
export function subtreeUnitIds(view, rootId) {
  const children = new Map();
  for (const unit of view?.units ?? []) {
    if (!unit.parent_unit_id) continue;
    if (!children.has(unit.parent_unit_id)) children.set(unit.parent_unit_id, []);
    children.get(unit.parent_unit_id).push(unit.unit_id);
  }
  const seen = new Set([rootId]);
  const stack = [rootId];
  while (stack.length) {
    for (const child of children.get(stack.pop()) ?? []) {
      if (!seen.has(child)) {
        seen.add(child);
        stack.push(child);
      }
    }
  }
  return seen;
}

/** `view` cut to a unit's subtree: its units, their positions and the people on them. */
export function viewSubset(view, unitId) {
  if (!unitId) return view;
  const within = subtreeUnitIds(view, unitId);
  const positions = (view.positions ?? []).filter((p) => within.has(p.unit_id));
  const ids = new Set(positions.map((p) => p.position_id));
  return {
    ...view,
    units: (view.units ?? []).filter((u) => within.has(u.unit_id)),
    positions,
    assignments: (view.assignments ?? []).filter((a) => ids.has(a.position_id)),
    vacancies: (view.vacancies ?? []).filter((id) => ids.has(id)),
  };
}

/** The units of two states with how many differences each has, the busiest first. */
export function unitChoices(before, after, items) {
  const names = new Map();
  for (const view of [before, after]) for (const u of view?.units ?? []) names.set(u.unit_id, u.name);
  const counts = new Map();
  for (const item of items ?? []) {
    if (item.unit_id) counts.set(item.unit_id, (counts.get(item.unit_id) ?? 0) + 1);
  }
  return [...names.entries()]
    .map(([id, name]) => ({ id, name, count: counts.get(id) ?? 0 }))
    .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
}

// ---------------------------------------------------------------------------
// Planned reorganizations
// ---------------------------------------------------------------------------

const plainId = (id) => String(id ?? '').toLowerCase().replaceAll('-', '');

/** What a reorganization's card offers `me` on `today`. Approval is another administrator's: the author's is a disabled button that says why. */
export function changeSetFlags(set, { me, today, soleAdmin = false }) {
  const open = set.state === 'draft' || set.state === 'pending';
  return {
    open,
    isDraft: set.state === 'draft',
    isPending: set.state === 'pending',
    isApplied: set.state === 'applied',
    isWithdrawn: set.state === 'withdrawn',
    authorIsMe: Boolean(me) && plainId(set.author_user_id) === plainId(me),
    canSubmit: set.state === 'draft',
    canApprove: set.state === 'pending' && (soleAdmin || !(Boolean(me) && plainId(set.author_user_id) === plainId(me))),
    approveBlockedByAuthor: set.state === 'pending' && !soleAdmin && Boolean(me) && plainId(set.author_user_id) === plainId(me),
    // The only administrator has nobody else to ask (owner decision 2026-09-30).
    selfApproval: set.state === 'pending' && soleAdmin && Boolean(me) && plainId(set.author_user_id) === plainId(me),
    canEdit: open,
    // An approved one until its day comes: its rows all start on that day, so they can be taken back whole.
    canWithdraw: open || (set.state === 'applied' && set.effective_date > today),
    datePassed: set.effective_date < today,
    days: daysBetween(today, set.effective_date),
  };
}

/**
 * Open ones first (soonest day first), then the approved ones still waiting for their day, then the closed: applied
 * ones whose day has come and the withdrawn (newest day first). An approved reorganization is not "closed" until
 * it has taken effect — its approver expects to see it.
 */
export function groupChangeSets(list, today) {
  const by = (states) => (list ?? []).filter((set) => states.includes(set.state));
  const soonest = (a, b) => a.effective_date.localeCompare(b.effective_date);
  const newest = (a, b) => b.effective_date.localeCompare(a.effective_date);
  const applied = by(['applied']);
  return {
    open: by(['draft', 'pending']).sort(soonest),
    upcoming: applied.filter((set) => set.effective_date >= today).sort(soonest),
    applied: applied.filter((set) => set.effective_date < today).sort(newest),
    withdrawn: by(['withdrawn']).sort(newest),
  };
}

const ERROR_CODES = new Set([
  'self_approval', 'change_set_conflict', 'change_set_invalid', 'change_set_state', 'effective_date_passed',
  'op_before_effective_date', 'not_found', 'too_many_ops', 'invalid_date', 'empty_field', 'invalid_value',
  'backdated_confirmation_required', 'not_valid_at', 'outside_validity', 'change_set_started', 'change_set_dependents',
  'change_set_no_undo',
]);

/** The message for a typed refusal: its own text when the screen knows the rule, the server's English otherwise. */
export function errorText(error, t) {
  if (!error) return '';
  if (ERROR_CODES.has(error.code)) {
    return t(`error.${error.code}`, { date: error.date ? formatDay(error.date) : '', id: error.id ?? '', field: error.field ?? '' });
  }
  return error.message || t('error.unknown');
}

/** The failed operations of a dry run, `{ index, text }`, for the card to list. */
export function failedOps(results, t) {
  return (results ?? [])
    .filter((r) => !r.ok)
    .map((r) => ({ index: r.index, text: errorText(r.error, t) }));
}

// ---------------------------------------------------------------------------
// Operations: the wire's shape and the edit mode's
// ---------------------------------------------------------------------------

const camel = (name) => name.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
const lowerFirst = (name) => name.charAt(0).toLowerCase() + name.slice(1);

/**
 * The operations a change set stores (`OrgWriteOp`: `{ temp_id, request: { UnitMoveRequest: { ... } } }`) as the
 * `{ kind, tempId?, ...camelCaseFields }` list `orgBatchRequest` and `orgChangeSetSaveRequest` take — the shape
 * the edit mode builds and reads. `kind` is the request without "Request" and with a lower-case first letter.
 */
export function opsFromWire(ops) {
  return (ops ?? []).map((op) => {
    const request = op.request ?? {};
    const [variant, fields] = Object.entries(request)[0] ?? ['', {}];
    const out = { kind: lowerFirst(variant.replace(/Request$/, '')) };
    const tempId = op.temp_id ?? op.tempId;
    if (tempId) out.tempId = tempId;
    for (const [key, value] of Object.entries(fields ?? {})) out[camel(key)] = value;
    return out;
  });
}

/** The day an operation takes effect (`from` or `validFrom`), or null for one without a day. */
export function opDay(op) {
  return op.from ?? op.validFrom ?? null;
}

/** The list without the operation at `index`; temp ids of the removed one stay unresolved for the rest, and the server says so. */
export function withoutOp(ops, index) {
  return ops.filter((_, i) => i !== index);
}

const OP_ACTIONS = {
  unitTypeCreate: 'unit_type_create', unitTypeUpdate: 'unit_type_update', unitTypeDelete: 'unit_type_delete',
  unitCreate: 'unit_create', unitUpdate: 'unit_update', unitMove: 'unit_move', unitEnd: 'unit_end',
  headSet: 'unit_head_set', deputyHeadsSet: 'unit_deputies_set',
  positionCreate: 'position_create', positionUpdate: 'position_update', positionMove: 'position_move', positionEnd: 'position_end',
  reportingLineSet: 'reporting_line_set', externalPersonCreate: 'external_person_create',
  assign: 'assignment_create', assignmentUpdate: 'assignment_update', assignmentEnd: 'assignment_end',
};

/** An operation of a reorganization as a line: what it does and to what, by name where the live structure knows it. */
export function opSummary(op, view, t) {
  const unit = (view?.units ?? []).find((u) => u.unit_id === op.unitId);
  const position = (view?.positions ?? []).find((p) => p.position_id === op.positionId);
  const name = op.name || unit?.name || position?.name || '';
  return t(`op.${OP_ACTIONS[op.kind] ?? 'unknown'}`, { name });
}
