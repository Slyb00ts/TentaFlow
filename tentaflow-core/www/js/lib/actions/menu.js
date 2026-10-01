// ===== File: lib/actions/menu.js — the "⋯" menu of every changeable item =====

import '/js/components/tf-menu.js';
import { focusTarget } from './fields.js';

let current = null;

/**
 * Opens a menu under `anchor`. `items` = [{ label, icon, danger, disabled,
 * reason, run } | { separator: true }]; a disabled item must say why in
 * `reason` — it is shown under the label and as its tooltip, so nobody is left
 * guessing. `subject` names what the menu acts on (its heading).
 *
 * Focus goes back to `anchor` before an item runs, so a window the item opens
 * remembers the anchor and returns to it. Returns the tf-menu element; opening
 * a menu on an anchor that already has one closes it instead.
 */
export function openActionMenu(anchor, items, subject) {
  if (current) {
    const same = current.anchor === anchor;
    current.close();
    if (same) return null;
  }
  const menu = document.createElement('tf-menu');
  if (subject) menu.setAttribute('heading', subject);
  const runners = new Map();
  items.forEach((item, index) => {
    if (item.separator) {
      menu.appendChild(document.createElement('tf-menu-divider'));
      return;
    }
    const el = document.createElement('tf-menu-item');
    el.setAttribute('label', item.label);
    el.setAttribute('action', String(index));
    if (item.icon) el.setAttribute('icon', item.icon);
    if (item.danger) el.setAttribute('danger', '');
    if (item.disabled) {
      el.setAttribute('disabled', '');
      if (item.reason) el.setAttribute('hint', item.reason);
    }
    runners.set(String(index), item.run);
    menu.appendChild(el);
  });
  // In the document BEFORE it opens: the panel is placed from its measured size, and its items only take
  // their real size once connected — opened first, it was placed by the width of empty items and stuck out
  // of the window at the right edge.
  document.body.appendChild(menu);
  menu.anchor = anchor;
  menu.setAttribute('open', '');
  current = menu;

  menu.addEventListener('close', () => {
    if (current === menu) current = null;
    menu.remove();
    if (anchor.isConnected) focusTarget(anchor).focus?.();
  });
  menu.addEventListener('action', (e) => runners.get(e.detail.action)?.());
  menu.querySelector('tf-menu-item:not([disabled]) > .tf-menu-item')?.focus();
  return menu;
}
