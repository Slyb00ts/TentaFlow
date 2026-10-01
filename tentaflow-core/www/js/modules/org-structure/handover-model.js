// =============================================================================
// File: modules/org-structure/handover-model.js
// Description: The pure part of the handover screen "Do przekazania" (mockup
//   F07): what the reasons are and who may use which, how the server's answer
//   becomes rows with a chosen taker, "hand everything to ...", the checks
//   before "Przekaż zaznaczone", the request that goes to the server and the
//   summary of its answer. No DOM and no transport — handover-tab.js draws it.
//
//   The server decides what a person holds, who may take each item and who is
//   proposed; this module only keeps the operator's choices. A choice the
//   server would refuse is caught here first (a missing taker, a taker who is
//   not among the eligible ones, no note) so the screen can say so, but the
//   server checks all of it again.
// =============================================================================

export const REASONS = ['departure', 'absence', 'project_removal'];

// The order of the groups on the screen.
export const CATEGORY_ORDER = ['task', 'test_item', 'membership', 'position', 'deputy'];

const CATEGORY_ICON = {
  task: 'check-circle',
  test_item: 'flask',
  membership: 'users',
  position: 'sitemap',
  deputy: 'user',
};

export const categoryIcon = (category) => CATEGORY_ICON[category] ?? 'list';

/**
 * The reasons this caller may use. A departure is the administrator's; an absence is the person's (their
 * manager's and the administrator's, which the server decides); a project removal exists only where the
 * screen was opened for a project.
 */
export function allowedReasons({ isAdmin, projectId }) {
  return REASONS.filter((reason) => {
    if (reason === 'departure') return Boolean(isAdmin) && !projectId;
    if (reason === 'project_removal') return Boolean(projectId);
    return !projectId;
  });
}

/** The reason to start with: what the caller asked for when it is allowed, else the first allowed one. */
export function initialReason(requested, allowed) {
  return allowed.includes(requested) ? requested : allowed[0] ?? 'absence';
}

const SORT = (a, b) => String(a).localeCompare(String(b), undefined, { sensitivity: 'base' });

/** Rows from the server's groups. Everything is selected and the proposed person is the taker to start with. */
export function rowsOf(groups) {
  const rows = [];
  for (const group of groups ?? []) {
    for (const item of group.items ?? []) {
      const suggestion = item.suggestion ?? null;
      rows.push({
        key: item.key,
        category: group.category,
        title: item.title,
        role: item.role,
        state: item.state ?? '',
        projectId: item.project_id ?? null,
        projectName: item.project_name ?? null,
        unitName: item.unit_name ?? null,
        validTo: item.valid_to ?? null,
        action: item.action,
        suggestion,
        eligible: Array.isArray(item.eligible_user_ids) ? new Set(item.eligible_user_ids) : null,
        blocked: item.blocked ?? null,
        // An item the server says cannot be handed over now is shown but never ticked.
        selected: !item.blocked,
        taker: suggestion?.user_id ?? '',
        manual: false,
      });
    }
  }
  return rows;
}

/** Rows grouped in screen order; a group with nothing in it is left out. */
export function grouped(rows) {
  return CATEGORY_ORDER
    .map((category) => ({ category, rows: rows.filter((r) => r.category === category) }))
    .filter((group) => group.rows.length > 0);
}

/** True when the row takes a person (as opposed to ending on its own). */
export const takesPerson = (row) => row.action !== 'end';

/** May `userId` take the row? Rows that only end take nobody. */
export function canTake(row, userId) {
  if (!takesPerson(row) || !userId) return false;
  return row.eligible ? row.eligible.has(userId) : true;
}

/** The people the row's select offers: the eligible ones, in the order the server sent them. */
export function takerOptions(row, takers) {
  return takers.filter((person) => canTake(row, person.user_id));
}

export const selectedRows = (rows) => rows.filter((row) => row.selected && !row.blocked);

/**
 * "Zastosuj do zaznaczonych": sets `userId` as the taker of every selected row that can be given to a person
 * and may take them. Answers how many were set and how many could not (a project the person is not in).
 */
export function applyToSelected(rows, userId) {
  let set = 0;
  let skipped = 0;
  for (const row of selectedRows(rows)) {
    if (!takesPerson(row)) continue;
    if (canTake(row, userId)) {
      row.taker = userId;
      row.manual = true;
      set += 1;
    } else {
      skipped += 1;
    }
  }
  return { set, skipped };
}

/** The temporary handovers of a person (the ones an absence made), newest first, with what became of their items. */
export function absenceRecords(records, today) {
  return (records ?? [])
    .filter((record) => record.reason === 'absence')
    .map((record) => {
      const count = (...statuses) => record.items.filter((item) => statuses.includes(item.status)).length;
      return {
        record,
        // Waiting = the work is with the takers and comes back on the return day.
        away: Boolean(record.return_date) && record.return_date > today && count('done') > 0,
        done: count('done'),
        returned: count('returned'),
        kept: count('kept'),
        failed: count('failed', 'pending'),
      };
    });
}

/** The reasons a request cannot be sent, as codes the screen turns into sentences. */
export function problems({ rows, note, reason, returnDate, today }) {
  const found = [];
  const chosen = selectedRows(rows);
  if (chosen.length === 0) found.push({ code: 'nothing_selected' });
  if (!String(note ?? '').trim()) found.push({ code: 'note_required' });
  if (reason === 'absence') {
    if (!returnDate) found.push({ code: 'return_required' });
    else if (today && returnDate <= today) found.push({ code: 'return_not_after_today' });
  }
  for (const row of chosen) {
    if (row.action === 'transfer' && !row.taker) found.push({ code: 'taker_required', key: row.key });
    if (row.taker && !canTake(row, row.taker)) found.push({ code: 'taker_not_eligible', key: row.key });
  }
  return found;
}

/** The apply request for the chosen rows. */
export function applyPayload({ userId, reason, projectId, date, returnDate, note, rows }) {
  return {
    userId,
    reason,
    projectId: reason === 'project_removal' ? projectId : null,
    date: reason === 'departure' ? date : null,
    returnDate: reason === 'absence' ? returnDate : null,
    note: String(note ?? '').trim(),
    items: selectedRows(rows).map((row) => ({
      key: row.key,
      takerUserId: takesPerson(row) && row.taker ? row.taker : null,
    })),
  };
}

const FAILED = new Set(['failed', 'not_started']);

/** What the apply answer says, in numbers and in the keys a retry would take. */
export function summarize(answer) {
  const items = answer?.items ?? [];
  const count = (...statuses) => items.filter((item) => statuses.includes(item.status)).length;
  return {
    done: count('done'),
    scheduled: count('scheduled'),
    failed: count('failed', 'not_started'),
    skipped: count('skipped'),
    failedKeys: items.filter((item) => FAILED.has(item.status)).map((item) => item.key),
    retryable: Boolean(answer?.handover_id) && items.some((item) => FAILED.has(item.status)),
    total: items.length,
  };
}

/** The sentence key of a reason code: the code itself, and where the screen has no sentence, the org rule of the same code. */
export function reasonKey(code) {
  return code ? `reasons.${code}` : null;
}

/** People as the select and the takers of "hand everything to" show them, sorted by name. */
export function sortedTakers(takers) {
  return [...(takers ?? [])].sort((a, b) => SORT(a.display_name, b.display_name) || SORT(a.user_id, b.user_id));
}

export function initials(name) {
  const parts = String(name ?? '').trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return '?';
  const letters = parts.length === 1 ? parts[0].slice(0, 2) : parts[0][0] + parts[parts.length - 1][0];
  return letters.toUpperCase();
}
