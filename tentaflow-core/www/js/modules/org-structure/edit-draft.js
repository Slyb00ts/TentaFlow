// =============================================================================
// File: modules/org-structure/edit-draft.js
// Description: The draft of the edit mode (mockup F02): the changes the
//   administrator has made, kept locally as a list of operations (edit-ops.js)
//   until "Zapisz zmiany" sends them ALL in one `orgBatchRequest` — one
//   transaction, one audit entry, nothing half-saved.
//
//   The canvas shows the draft, not the database: after every change the whole
//   list is sent as a DRY RUN (debounced, and at once when a change must be
//   checked before it is kept) and the structure it answers with is the preview.
//   A dry run also says, per operation, which rule refused it — that is what the
//   draft list and the inspector show on the right item.
//
//   Ids. What the draft creates has no real id until it is saved, and a dry run's
//   ids are thrown away with it (each run makes new ones), so every creating
//   operation carries a "tmp:" id and the preview is rewritten to use it: a card of
//   something the draft creates keeps its id from one preview to the next — the
//   selection and the expansion of the chart survive — and an action taken on it
//   is already in the draft's own terms.
//
//   Undo and redo work on the draft — every change, creating ones included, has
//   its inverse here because nothing has been written yet. Removing one row
//   ("Cofnij" in the list) removes what depended on it too.
// =============================================================================

import { subjectKey } from '/js/modules/org-structure/model.js';
import { addOp, isTempId, withDay, withoutOp } from '/js/modules/org-structure/edit-ops.js';

/** A rule of the structure refused: `code` is the stable name (`OrgOpError.code`). */
export class OrgWriteError extends Error {
  constructor(error) {
    super(error?.message ?? 'write refused');
    this.name = 'OrgWriteError';
    this.code = error?.code ?? 'internal';
    this.field = error?.field ?? null;
    this.entityId = error?.id ?? null;
    this.date = error?.date ?? null;
  }
}

export const BACKDATED = 'backdated_confirmation_required';
const DEBOUNCE_MS = 220;

const pick = (object, snake, camel) => object?.[snake] ?? object?.[camel];

const CREATES = { unitCreate: 'u', positionCreate: 'p', externalPersonCreate: 'e', assign: 'a' };

/**
 * @param {object} deps
 * @param {(kind: string, payload: object) => Promise<object>} deps.send the transport (`ApiBinary.one`)
 * @param {() => object | null} deps.getLive the structure on the effective day WITHOUT the draft
 * @param {(day: string) => Promise<boolean>} deps.askBackdated asks the administrator to confirm a change dated in the past
 * @param {string} deps.day the effective day
 */
export function createDraft({ send, getLive, askBackdated, day }) {
  let ops = [];
  let at = day;
  let confirmed = false;
  let counter = 0;
  let past = [];
  let future = [];
  let preview = null;
  let results = [];
  let timer = 0;
  let seq = 0;
  const listeners = new Set();

  const notify = () => listeners.forEach((fn) => fn());
  const snapshot = () => ({ ops, at });

  // ---- ids ------------------------------------------------------------------------

  // An assignment of the preview is written as the live one it comes from: a version made by an earlier row of the
  // draft has an id of the dry run's own, which will not exist when the draft is applied.
  function liveAssignmentId(id) {
    const shown = (preview?.view.assignments ?? []).find((a) => a.id === id);
    const live = getLive();
    if (!shown || !live) return id;
    const same = (live.assignments ?? []).find((a) => a.position_id === shown.position_id && subjectKey(a.subject) === subjectKey(shown.subject));
    return same ? same.id : id;
  }

  function inDraftTerms(op) {
    const out = { ...op };
    if (typeof out.assignmentId === 'string' && !isTempId(out.assignmentId)) out.assignmentId = liveAssignmentId(out.assignmentId);
    return out;
  }

  // The dry run's ids for what the draft creates, replaced by the temporary ones, everywhere they appear.
  function withTempIds(view, renames) {
    if (!renames.size) return view;
    const walk = (value) => {
      if (typeof value === 'string') return renames.get(value) ?? value;
      if (Array.isArray(value)) return value.map(walk);
      if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, walk(v)]));
      return value;
    };
    return walk(view);
  }

  // ---- the dry run ------------------------------------------------------------------

  const asOfWire = (list) => ({ ops: list, confirmBackdated: confirmed });

  function take(answer) {
    const list = answer.results ?? [];
    results = list.map((r) => ({
      index: r.index,
      ok: Boolean(r.ok),
      error: r.error ?? null,
      tempId: pick(r, 'temp_id', 'tempId') ?? null,
      createdId: pick(r, 'created_id', 'createdId') ?? null,
    }));
    const renames = new Map(results.filter((r) => r.tempId && r.createdId).map((r) => [r.createdId, r.tempId]));
    preview = answer.preview
      ? { view: withTempIds(answer.preview, renames), at: pick(answer, 'preview_at', 'previewAt') ?? at, warnings: withTempIds(answer.warnings ?? [], renames) }
      : null;
  }

  /** Sends the draft as a dry run now; the newest answer wins. Resolves when the preview is current. */
  function previewNow() {
    clearTimeout(timer);
    timer = 0;
    if (!ops.length) {
      seq += 1;
      preview = null;
      results = [];
      notify();
      return Promise.resolve();
    }
    seq += 1;
    const mine = seq;
    return send('orgBatchRequest', { ...asOfWire(ops), dryRun: true }).then((answer) => {
      if (mine !== seq) return;
      if (answer.error) throw new OrgWriteError(answer.error);
      take(answer);
      notify();
    });
  }

  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(() => {
      previewNow().catch(() => {});
    }, DEBOUNCE_MS);
    notify();
  }

  function record() {
    past.push(snapshot());
    future = [];
  }

  // ---- changes -----------------------------------------------------------------------

  function prepared(list) {
    let current = ops;
    let subject = null;
    for (const raw of list) {
      const op = inDraftTerms(raw);
      const prefix = CREATES[op.kind];
      if (prefix && !op.tempId) op.tempId = `tmp:${prefix}${(counter += 1)}`;
      if (op.createsSubject) subject = op.tempId;
      delete op.createsSubject;
      if (op.kind === 'assign' && op.subject?.id === null) op.subject = { ...op.subject, id: subject };
      current = addOp(current, withDay(op, at));
    }
    return current;
  }

  /**
   * Adds `list` (operations built from the preview) and checks them with a dry run before they are kept: an
   * operation the structure refuses is not added, and the refusal is thrown as `OrgWriteError`. A change dated
   * in the past is put to the administrator first, once for the day. Resolves with the creating operations it added.
   */
  async function addChecked(list) {
    const before = ops;
    record();
    ops = prepared(list);
    notify();
    const revert = async () => {
      ops = before;
      past.pop();
      await previewNow().catch(() => {});
    };
    try {
      await previewNow();
    } catch (err) {
      await revert();
      throw err;
    }
    // Only an operation this action added or changed is its fault; rows that already stood are not blamed on it.
    const known = new Set(before);
    const refused = results.find((r) => !r.ok && !known.has(ops[r.index]));
    if (!refused) return ops.filter((op) => op.tempId && !known.has(op));
    const error = new OrgWriteError(refused.error);
    if (error.code === BACKDATED && !confirmed) {
      ops = before;
      past.pop();
      if (await askBackdated(at)) {
        confirmed = true;
        await previewNow().catch(() => {});
        return addChecked(list);
      }
      await previewNow().catch(() => {});
      throw error;
    }
    await revert();
    throw error;
  }

  return {
    get ops() { return ops; },
    get at() { return at; },
    get results() { return results; },
    get preview() { return preview; },
    get dirty() { return ops.length > 0; },
    get canUndo() { return past.length > 0; },
    get canRedo() { return future.length > 0; },
    get confirmed() { return confirmed; },

    /** Operations the last dry run refused, with the operation and the rule. */
    errors() {
      return results.filter((r) => !r.ok).map((r) => ({ index: r.index, op: ops[r.index], error: new OrgWriteError(r.error) }));
    },

    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },

    addChecked,

    /** Starts from `list` on `day` (a reorganization opened for editing); the preview follows. */
    load(list, loadDay) {
      ops = list.map((op) => ({ ...op }));
      at = loadDay ?? at;
      confirmed = false;
      past = [];
      future = [];
      counter = ops.length + 1000;
      return previewNow().catch(() => {});
    },

    /** Drops the row at `index` and what depended on it. Returns how many operations went. */
    remove(index) {
      const next = withoutOp(ops, index);
      const dropped = ops.length - next.length;
      record();
      ops = next;
      schedule();
      return dropped;
    },

    undo() {
      if (!past.length) return;
      future.push(snapshot());
      ({ ops, at } = past.pop());
      schedule();
    },

    redo() {
      if (!future.length) return;
      past.push(snapshot());
      ({ ops, at } = future.pop());
      schedule();
    },

    /** Every operation moves to `newDay`; a past day is asked about again. */
    setDay(newDay) {
      record();
      at = newDay;
      confirmed = false;
      ops = ops.map((op) => withDay(op, newDay));
      schedule();
    },

    /** Empties the draft. */
    discard() {
      record();
      ops = [];
      confirmed = false;
      previewNow().catch(() => {});
    },

    /** Forgets the draft's history once it is saved: nothing left to undo. */
    clear() {
      ops = [];
      past = [];
      future = [];
      confirmed = false;
      counter = 0;
      previewNow().catch(() => {});
    },

    flush: previewNow,

    /**
     * Saves the draft in ONE batch: a dry run first, then the apply of the same list. Resolves with
     * `{ ok, applied, results, error, warnings }`; `ok` is false when a rule refused, and nothing was written.
     */
    async save() {
      const dry = await send('orgBatchRequest', { ...asOfWire(ops), dryRun: true });
      if (dry.error) return { ok: false, applied: false, error: dry.error, results: [], warnings: [] };
      take(dry);
      notify();
      if (!dry.ok) return { ok: false, applied: false, error: null, results, warnings: dry.warnings ?? [] };
      const answer = await send('orgBatchRequest', { ...asOfWire(ops), dryRun: false });
      const applied = Boolean(answer.applied);
      if (!applied) take(answer);
      notify();
      return { ok: applied, applied, error: answer.error ?? null, results: applied ? [] : results, warnings: answer.warnings ?? [] };
    },

    /** Records that the administrator confirmed a change dated in the past for this day. */
    confirm() {
      confirmed = true;
    },
  };
}
