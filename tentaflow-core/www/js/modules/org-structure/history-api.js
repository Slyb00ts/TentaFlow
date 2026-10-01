// =============================================================================
// File: modules/org-structure/history-api.js
// Description: The Historia tab's calls (OrgStructureBody::History*, ::ChangeSet*)
//   and the ONE place the planned reorganizations are read and written from the
//   screen — the edit mode uses these functions to "save as a planned
//   reorganization" (see history-bridge.js), so a change set is never built by
//   hand anywhere else.
//
//   A typed refusal (`self_approval`, `change_set_conflict`, ...) is an answer,
//   not an exception: every function resolves with `{ ok, error, ... }`, and
//   only a failure of the transport or of authorization rejects.
//   Operations travel as `{ kind, tempId?, ...camelCaseFields }`, the shape
//   `orgBatchRequest` takes (codec.js `orgWriteOpsToWire`), and come back in it.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { opsFromWire } from '/js/modules/org-structure/history-model.js';

const one = (name, payload) => ApiBinary.one(name, payload);

/** A change set of an answer with its operations turned into the edit mode's shape. */
function setOf(wire) {
  if (!wire) return null;
  return { ...wire, ops: opsFromWire(wire.ops) };
}

function setAnswer(body) {
  return {
    ok: Boolean(body.ok),
    error: body.error ?? null,
    changeSet: setOf(body.change_set),
    valid: Boolean(body.valid),
    results: body.results ?? [],
    warnings: body.warnings ?? [],
  };
}

/** The changes of the structure. `{ from?, to?, unitId?, offset?, limit? }` → `{ entries, total, personalVisible, today }`. */
export async function listHistory(params = {}) {
  const body = await one('orgHistoryListRequest', params);
  return {
    entries: body.entries ?? [],
    total: body.total ?? 0,
    personalVisible: Boolean(body.personal_visible),
    today: body.today ?? '',
  };
}

/** What differs between two days. `{ from, to, unitId? }` → `{ items, personalVisible }`. */
export async function diffDays(params) {
  const body = await one('orgHistoryDiffRequest', params);
  return { items: body.items ?? [], personalVisible: Boolean(body.personal_visible), from: body.from, to: body.to };
}

/** The planned reorganizations without their operations (`org.admin`). */
export async function listChangeSets() {
  const body = await one('orgChangeSetListRequest', {});
  return { items: body.items ?? [], today: body.today ?? '', soleAdmin: Boolean(body.sole_admin) };
}

/** One reorganization with its operations in the edit mode's shape. */
export async function getChangeSet(id) {
  return setAnswer(await one('orgChangeSetGetRequest', { id }));
}

/**
 * Creates (no `id`) or replaces a draft and dry-runs it. `{ id?, name, effectiveDate, ops }`. The draft is kept
 * even when some operation fails the dry run: `valid` and `results` say which.
 */
export async function saveChangeSet({ id = null, name, effectiveDate, ops = [] }) {
  return setAnswer(await one('orgChangeSetSaveRequest', { id, name, effectiveDate, ops }));
}

export async function submitChangeSet(id) {
  return setAnswer(await one('orgChangeSetSubmitRequest', { id }));
}

export async function approveChangeSet(id) {
  return setAnswer(await one('orgChangeSetApproveRequest', { id }));
}

export async function withdrawChangeSet(id) {
  return setAnswer(await one('orgChangeSetWithdrawRequest', { id }));
}

/** The structure on the day of the reorganization with and without it, and the differences. */
export async function previewChangeSet(id, unitId = null) {
  const body = await one('orgChangeSetPreviewRequest', { id, unitId });
  return {
    ...setAnswer(body),
    at: body.at ?? '',
    live: body.live ?? null,
    preview: body.preview ?? null,
    items: body.items ?? [],
  };
}
