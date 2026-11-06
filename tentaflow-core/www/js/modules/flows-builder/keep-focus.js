// ============ File: flows-builder/keep-focus.js — rebuild a host and give focus back to the control that had it ============

// A control keeps focus across a redraw when it carries a `data-focus-key`; the
// rebuilt control with the same key is found again and focused.
export function replaceKeepingFocus(host, build) {
  const focused = host.contains(document.activeElement) ? document.activeElement.closest('[data-focus-key]')?.dataset.focusKey : null;
  build();
  if (focused) host.querySelector(`[data-focus-key="${CSS.escape(focused)}"]`)?.querySelector('button, a')?.focus({ preventScroll: true });
}

// After an action whose control may be gone, focus must not fall to the page:
// when it is outside `win`, the first control that still exists is focused,
// else `fallback` (made focusable on the spot).
export function focusFirstOf(win, selectors, fallback) {
  if (win.contains(document.activeElement)) return;
  for (const selector of selectors) {
    const target = win.querySelector(selector)?.querySelector('button, a');
    if (target) { target.focus({ preventScroll: true }); return; }
  }
  if (fallback) {
    fallback.setAttribute('tabindex', '-1');
    fallback.focus({ preventScroll: true });
  }
}
