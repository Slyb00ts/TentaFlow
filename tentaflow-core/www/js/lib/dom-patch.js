// ===== File: lib/dom-patch.js — in-place DOM patching shared by polling screens (TentaNas, TentaBus) =====
//
// Every polling screen follows the rule the mockups state explicitly:
// "nigdy pełne odświeżenie całości" (research/03-ui-wzorce-mockupy.md). An
// `innerHTML =` on a poll destroys and recreates nodes whose content did not
// change, which is exactly what makes an alert, a KPI tile or a filter chip
// blink every few seconds. These helpers write only what actually differs.
//
// A leaf module with no imports: a screen shell and its tab modules both
// import it, so exporting these from a shell would create an import cycle
// whose evaluation order depends on which module is entered first.

/// Sets (or removes) one attribute, but only when the value really changes:
/// `setAttribute` with an identical value still runs the element's
/// `attributeChangedCallback`, and a tf-* component re-renders its insides
/// there. `null`/`false`/`''` remove the attribute.
export function setAttr(el, name, value) {
  if (!el) return;
  if (value == null || value === false || value === '') {
    if (el.hasAttribute(name)) el.removeAttribute(name);
    return;
  }
  const next = value === true ? '' : String(value);
  if (el.getAttribute(name) !== next) el.setAttribute(name, next);
}

/// Hands a tf-table its `rows` only when they differ from what it was last
/// given. `rows =` is a full table render — every cell, every row-action
/// button and any hover on them is rebuilt — so a poll that read the same
/// thing must not assign at all. The rows are compared as JSON, which is why
/// they must be plain data (an object kept for a row action, like pool-detail's
/// `_prop`, is plain too).
export function setRowsIfChanged(table, rows) {
  if (!table) return false;
  const sig = JSON.stringify(rows);
  if (table.__tfRowsSig === sig) return false;
  table.__tfRowsSig = sig;
  table.rows = rows;
  return true;
}

export function setText(el, text) {
  if (!el) return;
  const next = String(text);
  if (el.textContent === next) return;
  el.__tfHtml = null;
  el.textContent = next;
}

/// Writes `html` into `host` only when it differs from what was last written
/// there. Returns true when the DOM was actually replaced, so callers
/// re-attach their listeners exactly then — and never on an unchanged poll,
/// which now touches no node at all.
///
/// The cache is only valid while EVERY write to `host` goes through here: a
/// direct `host.innerHTML =` somewhere else leaves `__tfHtml` describing
/// markup that is no longer on screen, and the next patch with that same
/// string is then skipped as a no-op. One host, one writer.
export function patchHtml(host, html) {
  if (!host) return false;
  if (host.__tfHtml === html) return false;
  host.__tfHtml = html;
  host.innerHTML = html;
  return true;
}

/// Patches a KEYED row of cards in place. Each item is compared against the
/// markup last written for ITS OWN key, so one node's uptime ticking rebuilds
/// that one card and leaves every sibling — and any selection or hover inside
/// it — untouched. `patchHtml` cannot do this: it compares the JOINED string,
/// so a single value crossing a rounding boundary rebuilds the whole grid.
///
/// `items` is `[{ key, html }]`, and each `html` should have exactly ONE root
/// element — that element is what gets kept; an item whose html yields none
/// contributes no node, and is remembered as such so it is not re-parsed on
/// every poll. The children end up in `items` order; a key that disappears
/// takes its element with it, and a key that comes back is built fresh rather
/// than resurrected, so no stale card can linger and no node can end up with
/// two. Anything in `host` the key set does not own is swept, INCLUDING a text
/// node some other writer left there — the element-only ordering walk below
/// can never reach one.
///
/// Returns true when any element was created, moved or removed, so callers
/// that wire per-card listeners re-wire exactly then.
export function patchKeyedList(host, items) {
  if (!host) return false;
  const prev = host.__tfKeyed instanceof Map ? host.__tfKeyed : null;
  const next = new Map();
  let changed = false;
  for (const item of items) {
    const key = String(item.key);
    const html = String(item.html);
    const old = prev?.get(key);
    // Same key, same markup, still ours: keep the element exactly as it is.
    // `parentNode === host` is what makes a mixed-writer host safe. Once some
    // other writer has cleared this host the cached elements are detached, and
    // putting one back would return to the screen the very node that writer
    // replaced; the key is rebuilt instead.
    if (old && old.html === html && (old.el === null || old.el.parentNode === host)) {
      next.set(key, old);
      continue;
    }
    const box = document.createElement('div');
    box.innerHTML = html;
    const el = box.firstElementChild;
    // A key whose html yields no root element is cached too (`el: null`), so a
    // caller that keeps producing it pays this parse once instead of once per
    // poll for as long as the screen is open.
    next.set(key, { el, html });
    if (el) changed = true;
  }
  // Everything the new key set does not own goes FIRST — before the ordering
  // walk, not after it. Removing afterwards parked the cursor on an element
  // that was already doomed: nothing ever matched it, so every survivor behind
  // a changed card was re-inserted in front of it (six insertBefore calls on a
  // six-node fleet where one is needed). Sweeping childNodes rather than only
  // elements also clears what the walk below cannot see — a text node left by
  // a `setText` or a raw `textContent =` on this same host, which no later
  // pass could ever reach. A host never written by key lands here too: it owns
  // nothing, so all of it goes.
  const keep = new Set();
  for (const { el } of next.values()) if (el) keep.add(el);
  for (const child of [...host.childNodes]) {
    if (keep.has(child)) continue;
    child.remove();
    changed = true;
  }
  // Put survivors and newcomers in order, moving only what is actually out of
  // place: the ordinary poll — same nodes, same order — matches `cursor` on
  // every step and touches no node at all. Only nodes the new key set owns are
  // left in `host` now, so the walk always ends with `cursor` at null and
  // there is nothing to clean up behind it.
  let cursor = host.firstElementChild;
  for (const { el } of next.values()) {
    if (!el) continue;
    if (cursor === el) { cursor = el.nextElementSibling; continue; }
    host.insertBefore(el, cursor);
    changed = true;
  }
  host.__tfKeyed = next;
  // This host is now written per key rather than as one string; a leftover
  // `patchHtml` cache would describe markup that is no longer on screen.
  host.__tfHtml = null;
  return changed;
}

/// Paints a row of `<tf-stat-card>`s in place. The elements are created once
/// and every later poll writes only the attributes that changed, so a tile
/// keeps its identity (and its click handler) while its numbers move.
/// Returns true when the row was (re)built and the caller has to wire it.
export function paintStatCards(host, specs) {
  if (!host) return false;
  const sig = specs.map((s) => s.key).join('|');
  const built = host.__tfCards !== sig;
  if (built) {
    host.__tfCards = sig;
    host.replaceChildren(...specs.map((s) => {
      const el = document.createElement('tf-stat-card');
      el.dataset.kpi = s.key;
      if (s.className) el.className = s.className;
      return el;
    }));
  }
  specs.forEach((s, i) => {
    const el = host.children[i];
    for (const [name, value] of Object.entries(s.attrs)) setAttr(el, name, value);
  });
  return built;
}

// Writes a job log into its <pre> without disturbing the reader. A log only
// grows while its job runs, so the new tail is APPENDED as a text node — the
// text already on screen, and with it the scroll position of someone reading
// an earlier line, is left alone. A reader parked at the bottom is kept at
// the bottom so the tail stays in view. Anything but growth rewrites it.
export function paintJobLog(pre, lines) {
  if (!pre) return;
  const text = (lines || []).join('\n');
  const prev = pre.__tfLog ?? pre.textContent;
  if (text === prev) return;
  const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 4;
  if (prev && text.startsWith(prev)) pre.appendChild(document.createTextNode(text.slice(prev.length)));
  else pre.textContent = text;
  pre.__tfLog = text;
  if (atBottom) pre.scrollTop = pre.scrollHeight;
}

// A one-element slot for what appears and disappears with the data (a state
// chip, a media badge, a scan bar, a hint): the element stays the same node
// for as long as it stays on, so its attributes can be patched in place.
// `hidden` would not do — badge and component CSS set `display`, which wins
// over the UA's `[hidden]` rule — and the wrapper is `display: contents` so it
// adds no box to the flex rows it sits in. Put `SLOT` on the wrapper element.
export const SLOT = 'style="display:contents"';
export function slotEl(slot, on, key, html) {
  patchKeyedList(slot, on ? [{ key, html }] : []);
  return on ? slot.firstElementChild : null;
}

// `classList.toggle` rewrites the class attribute even when nothing changes;
// an unchanged node should see no mutation on a poll.
export function setClass(el, name, on) {
  if (el && el.classList.contains(name) !== Boolean(on)) el.classList.toggle(name, Boolean(on));
}
