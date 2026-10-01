// =============================================================================
// File: modules/org-structure/history-bridge.js
// Description: The seam between the Historia tab and the edit mode, for the
//   planned reorganization ("Edytuj w trybie edycji", "Nowa reorganizacja").
//
//   Two directions, both small:
//
//   1. Historia -> edit mode. The edit mode registers ONE handler with
//      `registerChangeSetEditor({ open })`. `open({ id, name, effectiveDate, ops })`
//      must switch to the edit mode with that reorganization loaded: `ops` are
//      `{ kind, tempId?, ...camelCaseFields }` (the shape `orgBatchRequest` takes),
//      `id` is null for a new one. It returns (a promise of) `true` when it took
//      the reorganization; anything else makes the tab open its own basic
//      editor (name, day, the list of operations), so the button never does nothing.
//
//   2. Edit mode -> Historia. To store what was edited as a reorganization the
//      edit mode calls `saveChangeSet({ id, name, effectiveDate, ops })` from
//      history-api.js (drafts are kept even when some operation fails the dry
//      run; the answer says which) and, to hand the person back to the list of
//      reorganizations, `showHistoryTab()` here. `onChangeSetsChanged(fn)` lets
//      the tab refresh when a change set was saved from the edit mode.
//
//   Nothing here knows how the edit mode works, and the edit mode needs nothing
//   of the tab beyond these five functions.
// =============================================================================

let editor = null;
const listeners = new Set();

/** The edit mode announces how it opens a reorganization. `null` unregisters. */
export function registerChangeSetEditor(next) {
  editor = next && typeof next.open === 'function' ? next : null;
}

export const changeSetEditorAvailable = () => editor !== null;

/**
 * Hands a reorganization to the edit mode. Resolves `true` when the edit mode took it, `false` when there is none
 * (or it declined), so the caller can fall back.
 */
export async function openChangeSetInEditor(request) {
  if (!editor) return false;
  return (await editor.open(request)) === true;
}

/** Tells the Historia tab a change set was saved, submitted, withdrawn... elsewhere; it reads the list again. */
export function notifyChangeSetsChanged() {
  for (const listener of listeners) listener();
}

/** The tab subscribes here; returns the unsubscribe. */
export function onChangeSetsChanged(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function selectTab(id) {
  const tabs = document.getElementById('org-tabs');
  if (!tabs) return false;
  tabs.setAttribute('value', id);
  // The screen listens for the tab strip's own change event; setting the attribute does not raise it.
  tabs.dispatchEvent(new CustomEvent('change', { detail: { value: id }, bubbles: true }));
  return true;
}

/** Shows the Historia tab. */
export const showHistoryTab = () => selectTab('history');

/** Shows the Drzewo tab (where the edit mode lives). */
export const showTreeTab = () => selectTab('tree');
