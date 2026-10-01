// =============================================================================
// File: modules/org-structure/import-model.js
// Description: Pure model of the import panel: which file a name is, the
//   request a run sends, the error cards of a dry-run report (with the
//   decisions the server accepts for each), what "replace" would end, whether
//   "Zapisz wszystko" may be pressed, and which positions of the preview are
//   new, changed or to be corrected. No DOM and no transport. Wire fields are
//   read in their snake_case spelling, as in model.js.
// =============================================================================

/**
 * The dashboard socket carries a frame of at most 1 MiB and the file travels inside one request,
 * so a bigger file is refused before it is sent — with the server's own `file_too_large` sentence.
 */
export const MAX_FILE_BYTES = 900 * 1024;

export const RESOLUTIONS = ['use_suggested_login', 'leave_vacant', 'skip_row'];

// Errors that are about the run and not about a row of the file: skipping a row does not answer them.
const NOT_SKIPPABLE = new Set(['backdated_confirmation_required', 'resolution_not_applicable', 'replace_matches_nothing']);

/** `csv` or `xlsx` by the file name's extension; null for anything else. */
export function formatOfFile(name) {
  const match = /\.([a-z0-9]+)$/i.exec(String(name ?? ''));
  const extension = match ? match[1].toLowerCase() : '';
  return extension === 'csv' || extension === 'xlsx' ? extension : null;
}

/** The payload of a dry run, an apply and an error export — the same for all three, so they cannot disagree. */
export function runPayload({ format, bytes, mode, asOf, confirmBackdated, confirmEnded = false, resolutions }) {
  return {
    format,
    bytes,
    mode,
    asOf: asOf || null,
    confirmBackdated: Boolean(confirmBackdated),
    confirmEnded: Boolean(confirmEnded),
    // `login` pins the login the administrator saw suggested, so a later run cannot swap in another one.
    resolutions: resolutions.map(({ row, action, login }) => ({ row, action, ...(login ? { login } : {}) })),
  };
}

/** The decisions after `row` was decided as `action` (one decision per row; deciding again replaces it). */
export function withResolution(resolutions, row, action, login = null) {
  return [...resolutions.filter((r) => r.row !== row), { row, action, ...(login ? { login } : {}) }];
}

export function withoutResolution(resolutions, row) {
  return resolutions.filter((r) => r.row !== row);
}

/** The decisions the server would accept for one error of a report. */
export function actionsFor(issue) {
  if (!issue.row || NOT_SKIPPABLE.has(issue.kind)) return [];
  const actions = [];
  if (issue.kind === 'unknown_person' && issue.suggestion) actions.push('use_suggested_login');
  if (issue.kind === 'unknown_person' || issue.kind === 'ambiguous_person') actions.push('leave_vacant');
  actions.push('skip_row');
  return actions;
}

/** "88–89", "131, 134" or "47": the rows an issue is about, as the spreadsheet numbers them. */
export function rowsLabel(issue) {
  const rows = [...new Set([...(issue.rows ?? []), issue.row].filter(Boolean))].sort((a, b) => a - b);
  if (rows.length < 2) return rows.length ? String(rows[0]) : '';
  const consecutive = rows.every((n, i) => i === 0 || n === rows[i - 1] + 1);
  return consecutive ? `${rows[0]}–${rows[rows.length - 1]}` : rows.join(', ');
}

// The server lists this among the errors of a replace that ends things and was not confirmed. It is not a
// mistake of the file but the confirmation step: the screen asks for it when applying, so it never blocks the
// save and is never drawn as a card.
const CONFIRMATION_KINDS = new Set(['ended_confirmation_required']);

/** The problems the administrator has to resolve: the report's errors without the confirmation step. */
export function blockingErrors(report) {
  return (report?.errors ?? []).filter((issue) => !CONFIRMATION_KINDS.has(issue.kind));
}

/** The error cards of a report, each with the decisions offered and the one already taken. */
export function errorCards(report, resolutions) {
  return blockingErrors(report).map((issue, index) => ({
    key: `${index}:${issue.row}:${issue.kind}`,
    issue,
    actions: actionsFor(issue),
    decided: resolutions.find((r) => r.row === issue.row)?.action ?? null,
  }));
}

/** The row of a report that has the number `row`, when the report carries it. */
export function reportRow(report, row) {
  return (report?.rows ?? []).find((r) => r.row === row) ?? null;
}

/** The items a replace would end, as the report lists them: kind, code, name and who loses a seat. */
export function endedItems(report) {
  return (report?.ended ?? []).map((item) => ({
    kind: String(item.kind ?? ''),
    code: item.code ?? '',
    name: item.name ?? '',
    holders: (item.holders ?? []).map((h) => (typeof h === 'string' ? h : h?.display_name ?? h?.name ?? '')).filter(Boolean),
  }));
}

/** People who lose a seat when the replace is applied, each counted once. */
export function seatsLost(report) {
  return new Set(endedItems(report).flatMap((item) => item.holders)).size;
}

/** What "replace" would end, from the counts of the dry run — the only honest number, the server's own. */
export function replaceImpact(report) {
  const counts = report?.counts ?? {};
  const units = counts.units_ended ?? 0;
  const positions = counts.positions_ended ?? 0;
  const assignments = counts.assignments_ended ?? 0;
  return { units, positions, assignments, total: units + positions + assignments };
}

/**
 * How many problems the report lists. The server also counts rows in error (`counts.errors`), which can be more
 * than the problems — a cycle is one problem on two rows — so the screen shows one number everywhere: the list.
 */
export function errorCount(report) {
  if (!report?.errors) return report?.counts?.errors ?? 0;
  return blockingErrors(report).length;
}

/** True while a dry-run report has something the administrator must resolve before saving. */
export function unresolvedErrors(report) {
  return errorCount(report) > 0;
}

/**
 * Whether "Zapisz wszystko" may be pressed: a report of THIS file, mode and day, without errors, that
 * has not been applied. What a replace ends is confirmed in a step of its own after the press.
 */
export function canApply({ report, busy }) {
  if (!report || busy || report.applied || report.file_error) return false;
  return !unresolvedErrors(report);
}

/** True when applying this report ends something, so the server wants `confirm_ended` and the screen asks first. */
export function needsEndedConfirmation(report, mode) {
  const asked = (report?.errors ?? []).some((issue) => CONFIRMATION_KINDS.has(issue.kind));
  return mode === 'replace' && (asked || replaceImpact(report).total > 0 || endedItems(report).length > 0);
}

/** Position ids of the preview by what the file does to them; an error wins over a change. */
export function previewMarks(report) {
  const marks = new Map();
  const rank = { error: 3, changed: 2, added: 1 };
  for (const row of report?.rows ?? []) {
    if (!row.position_id || !(row.status in rank)) continue;
    if ((rank[marks.get(row.position_id)] ?? 0) < rank[row.status]) marks.set(row.position_id, row.status);
  }
  return marks;
}

/** Unit ids of the preview by the strongest thing the file does in them (its own row or any of its positions). */
export function previewUnitMarks(report) {
  const unitOfPosition = new Map((report?.preview?.positions ?? []).map((p) => [p.position_id, p.unit_id]));
  const rank = { error: 3, changed: 2, added: 1 };
  const marks = new Map();
  for (const row of report?.rows ?? []) {
    if (!(row.status in rank)) continue;
    for (const unit of new Set([row.unit_id, unitOfPosition.get(row.position_id)].filter(Boolean))) {
      if ((rank[marks.get(unit)] ?? 0) < rank[row.status]) marks.set(unit, row.status);
    }
  }
  return marks;
}

/** The rows of a report for one of the list tabs: errors are cards, the others are these rows. */
export function rowsByStatus(report, status) {
  return (report?.rows ?? []).filter((r) => r.status === status);
}
