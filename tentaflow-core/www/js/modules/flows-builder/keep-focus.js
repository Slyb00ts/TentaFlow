// ============ File: flows-builder/keep-focus.js — rebuild a host and give focus back to the control that had it ============

// A control keeps focus across a redraw when it carries a `data-focus-key`; the
// rebuilt control with the same key is found again and focused.
export function replaceKeepingFocus(host, build) {
  const focused = host.contains(document.activeElement) ? document.activeElement.closest('[data-focus-key]')?.dataset.focusKey : null;
  build();
  if (focused) host.querySelector(`[data-focus-key="${CSS.escape(focused)}"]`)?.querySelector('button, a')?.focus({ preventScroll: true });
}
