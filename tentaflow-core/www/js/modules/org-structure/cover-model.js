// =============================================================================
// File: modules/org-structure/cover-model.js
// Description: Pure model of absences, deputies and visibility (docs
//   ORG_STRUCTURE_PLAN.md §1, §6.3): the rows of the profile sections and the
//   Widoczność tab, the day arithmetic the absence window needs, and the
//   badges the tree gets. No DOM and no transport, so every rule is tested
//   without a page. Wire fields are read in their snake_case spelling, as in
//   model.js.
//
//   An end date on the wire is EXCLUSIVE (`[from, to)`, like every end date of
//   the structure). People think in "last day of the leave", so the windows
//   talk in inclusive last days and this module converts at the edge — the
//   only place the two meet.
// =============================================================================

import { I18n } from '/js/i18n.js';
import { addDays, daysBetween, formatDay, isIsoDay } from '/js/lib/date-format.js';
import { subjectKey } from '/js/modules/org-structure/model.js';

/** The last day a `[from, to)` interval covers; `null` for an open end. */
export function lastDayOf(validTo) {
  return validTo ? addDays(validTo, -1) : null;
}

/** The exclusive end the wire wants for a last covered day; `null` for none. */
export function exclusiveEnd(lastDay) {
  return lastDay ? addDays(lastDay, 1) : null;
}

/** "20.10.2026 – 24.10.2026", "14.12.2026", "od 20.10.2026" (open end), in the UI language's date format. */
export function rangeText(validFrom, validTo, { from = 'from', open = 'open' } = {}) {
  const last = lastDayOf(validTo);
  if (!last) return `${from} ${formatDay(validFrom)}`.trim() || open;
  if (last === validFrom) return formatDay(validFrom);
  return `${formatDay(validFrom)} – ${formatDay(last)}`;
}

/** Whole calendar days an absence covers, counting weekends; `null` when it has no end. */
export function dayCount(absence) {
  if (!absence.valid_to) return null;
  return daysBetween(absence.valid_from, absence.valid_to);
}

/** Where an absence stands on `today`. */
export function phaseOf(item, today) {
  if (item.valid_to && item.valid_to <= today) return 'past';
  if (item.valid_from > today) return 'upcoming';
  return 'current';
}

/** Absences in reading order: running first, then coming ones by date, then the past newest first. */
export function absenceRows(absences, today) {
  const order = { current: 0, upcoming: 1, past: 2 };
  return [...(absences ?? [])]
    .map((absence) => ({ absence, phase: phaseOf(absence, today), manual: absence.source === 'manual' }))
    .sort((a, b) => order[a.phase] - order[b.phase]
      || (a.phase === 'past' ? b.absence.valid_from.localeCompare(a.absence.valid_from) : a.absence.valid_from.localeCompare(b.absence.valid_from))
      || a.absence.id.localeCompare(b.absence.id));
}

/** 'all', 'approvals', 'escalations' or 'project' for a deputy scope of the wire. */
export function scopeKind(scope) {
  const text = String(scope ?? '');
  return text.startsWith('project:') ? 'project' : text;
}

/** Deputies in reading order: in force first, then coming ones, by start. */
export function deputyRows(list, today) {
  const order = { current: 0, upcoming: 1, past: 2 };
  return [...(list ?? [])]
    .map((deputy) => ({ deputy, phase: phaseOf(deputy, today) }))
    .sort((a, b) => order[a.phase] - order[b.phase]
      || a.deputy.valid_from.localeCompare(b.deputy.valid_from)
      || a.deputy.id.localeCompare(b.deputy.id));
}

/**
 * What the tree badges: the subject keys of people away on the day and of people who are a deputy in force.
 * An absence has no reason, so none reaches here.
 */
export function treeBadges(availability) {
  const absentKeys = new Set((availability?.absent_user_ids ?? []).map((id) => subjectKey({ kind: 'user', id })));
  const coveringKeys = new Set((availability?.deputies ?? []).map((d) => subjectKey({ kind: 'user', id: d.deputy_user_id })));
  return { absentKeys, coveringKeys };
}

/** People of the structure with an account, for the picker: one entry per user, by name. */
export function personOptions(view) {
  const seen = new Map();
  for (const a of view?.assignments ?? []) {
    if (a.subject?.kind !== 'user' || seen.has(a.subject.id)) continue;
    seen.set(a.subject.id, { id: a.subject.id, name: a.display_name || I18n.t('org_structure.unknown_person') });
  }
  return [...seen.values()].sort((a, b) => a.name.localeCompare(b.name));
}

const VERDICT_TONE = { all: 'yes', subtree: 'yes', direct: 'yes', own: 'part', none: 'no' };

/** `yes` / `part` / `no`, the colour a verdict is drawn in. */
export function verdictTone(verdict) {
  return VERDICT_TONE[verdict] ?? 'no';
}

/** The order the areas are shown in. */
export const AREA_ORDER = ['structure', 'utilization', 'absence_dates', 'position_history', 'everyone_else'];

/**
 * The rows of "X widzi": each area with the message keys of its verdict and rule and how many people
 * the verdict reaches (`utilization` and `absence_dates` the subtree).
 */
export function visibilityRows(response) {
  const subtree = response?.subtree ?? [];
  const rows = [...(response?.rows ?? [])].sort((a, b) => AREA_ORDER.indexOf(a.area) - AREA_ORDER.indexOf(b.area));
  return rows.map((row) => {
    const people = row.area === 'utilization' || row.area === 'absence_dates' ? subtree : [];
    const reach = row.verdict === 'subtree' || row.verdict === 'direct' ? people : [];
    return {
      area: row.area,
      verdict: row.verdict,
      rule: row.rule,
      tone: verdictTone(row.verdict),
      people: reach,
      // Own data plus people below: the mockup's "daty, powód u 7 osób" — the amount, not the list.
      count: reach.length,
    };
  });
}

/** Viewers of one person's data in reading order: the person, then by rule strength and name. */
export function viewerRows(response, subjectId) {
  const rank = { owner: 0, primary_manager: 1, supervisor: 2, administrator: 3 };
  return [...(response?.viewers ?? [])]
    .map((v) => ({ ...v, self: v.user_id === subjectId }))
    .sort((a, b) => (rank[a.rule] ?? 9) - (rank[b.rule] ?? 9)
      || (a.display_name || '').localeCompare(b.display_name || ''));
}

/** Two-letter initials for the small avatar; the same rule as the tree cards. */
export function initials(name) {
  const parts = String(name ?? '').trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return '?';
  return parts.slice(0, 2).map((part) => Array.from(part)[0]).join('').toUpperCase();
}

/**
 * Turns what the absence window collected into the wire's fields, or names the field that is wrong.
 * `values` = { from, last, kind } with `last` the inclusive last day (empty = no end).
 */
export function absenceFields(values) {
  if (!isIsoDay(values.from)) return { error: 'from' };
  if (values.last && !isIsoDay(values.last)) return { error: 'last' };
  if (values.last && values.last < values.from) return { error: 'order' };
  return {
    fields: {
      validFrom: values.from,
      validTo: exclusiveEnd(values.last),
      kind: values.kind,
    },
  };
}

/** Which wire fields an edit changed, so the request names only those (and `clear` for the emptied ones). */
export function absencePatch(before, values) {
  const next = absenceFields(values);
  if (next.error) return next;
  const { fields } = next;
  const patch = {};
  const clear = [];
  if (fields.validFrom !== before.valid_from) patch.validFrom = fields.validFrom;
  if ((fields.validTo ?? null) !== (before.valid_to ?? null)) {
    if (fields.validTo) patch.validTo = fields.validTo;
    else clear.push('valid_to');
  }
  if (fields.kind !== before.kind) patch.kind = fields.kind;
  return { patch, clear, changed: Object.keys(patch).length + clear.length > 0 };
}
