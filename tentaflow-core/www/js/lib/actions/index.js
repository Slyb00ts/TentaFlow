// ===== File: lib/actions/index.js — the shared action module: menu, windows, undo toast, bulk bar =====
//
//   import { openActionMenu, openAssignWindow, ... } from '/js/lib/actions/index.js';
//
// One mechanism for every "⋯" of the dashboard (org structure first, Project
// Studio and others next): a screen supplies items, people and callbacks; the
// menu, the windows and their behaviour (validation, busy state, typed server
// errors, focus) are here once.

export { openActionMenu } from './menu.js';
export { openAssignWindow, openHandoverWindow, openEditWindow, openMoveWindow, openConfirmWindow } from './windows.js';
export { showUndoToast } from './toast.js';
export { attachBulkBar } from './bulk.js';
