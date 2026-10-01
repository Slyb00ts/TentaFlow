// =============================================================================
// File: modules/org-structure/history-asof.js
// Description: The "Stan na" control of the Drzewo tab: the chart can show the
//   structure on another day — a planned one included — with what differs from
//   today marked on the cards (new, changed). It is the tree tab's own chart fed
//   another day's answer, exactly as the edit mode feeds it its effective day; the
//   marks are written on the model by the tab's `decorate` hook. While a day other
//   than today is shown the screen's refreshes are held back (`active()`), and the
//   edit mode resets it first, so the two never fight over the chart.
//   Reading a past day is the position history of every person: the server
//   numbers the people for anyone but an administrator, so nothing is hidden here.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { TfToast } from '/js/components/tf-toast.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import { openActionMenu } from '/js/lib/actions/index.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { dateFormatHint, formatDay, isIsoDay } from '/js/lib/date-format.js';
import { decorateModel, diffCounts, diffMarks, markers } from '/js/modules/org-structure/history-model.js';
import { diffDays, listChangeSets, listHistory, previewChangeSet } from '/js/modules/org-structure/history-api.js';
import { showTreeTab } from '/js/modules/org-structure/history-bridge.js';

const t = (key, params) => I18n.t(`org_structure.history.${key}`, params);

// A day asked for from another tab (the Historia tab's "Pokaż w drzewie"); the control takes it when it is
// mounted, or at once when it already is.
let requested = null;
const listeners = new Set();

/** Shows the Drzewo tab on `day`. */
export function requestTreeAsOf(day) {
  // Applied before the tab is shown, so the refresh the screen starts on showing it already finds another day on the chart.
  if (listeners.size > 0) for (const listener of listeners) listener(day);
  else requested = day;
  showTreeTab();
}

/**
 * @param {HTMLElement} host the toolbar slot of the tree tab (the button)
 * @param {HTMLElement} note the row under the toolbar (what is marked, and the way back)
 * @param {object} api given by the tree tab: `apply(answer)` takes a structure answer into the chart,
 *   `setDecorate(fn | null)` sets the hook that marks the model
 * @param {{ view: object, unitTypes: Array, myPermissions: Array }} base today's structure, as the screen last read it
 */
export function mountAsOf(host, api, base, note) {
  let today = base;
  let day = null;
  // The reorganization on show when the chart is drawing a plan's preview rather than a day of the structure.
  let previewName = '';
  let counts = null;
  let plannedDays = [];
  let openSets = [];
  let token = 0;

  function render() {
    const label = !day
      ? t('asof_today')
      : previewName
        ? t('asof_preview_label', { name: previewName, date: formatDay(day) })
        : t('asof_label', { date: formatDay(day) });
    host.innerHTML = `<tf-button variant="secondary" icon="calendar" data-act="asof">${escapeHtml(label)}</tf-button>`;
    note.hidden = !day;
    if (!day) {
      note.replaceChildren();
      return;
    }
    const summary = counts
      ? `<tf-chip variant="outline" status="warn" icon="calendar">${escapeHtml(t('asof_diff', { added: counts.added, changed: counts.changed + counts.removed }))}</tf-chip>`
      : '';
    note.innerHTML = `${summary}<tf-button variant="ghost" size="sm" icon="x" data-act="asof-back">${escapeHtml(t('asof_back'))}</tf-button>`;
  }

  async function loadPlannedDays() {
    try {
      const { entries, today: at } = await listHistory({ from: today.view.at, limit: 200 });
      plannedDays = markers(entries, [], at).filter((m) => m.planned).map((m) => m.day);
    } catch {
      plannedDays = [];
    }
  }

  // A plan that is not approved has written nothing, so it can only be looked at through its dry run.
  async function loadOpenSets() {
    if (!today.myPermissions.includes('org.admin')) {
      openSets = [];
      return;
    }
    try {
      openSets = (await listChangeSets()).items.filter((set) => set.state === 'draft' || set.state === 'pending');
    } catch {
      openSets = [];
    }
  }

  function reset() {
    token += 1;
    day = null;
    previewName = '';
    counts = null;
    api.setDecorate(null);
    api.apply(today);
    render();
  }

  async function apply(next) {
    if (!isIsoDay(next)) return;
    if (next === today.view.at) {
      reset();
      return;
    }
    // Set before the first await: a refresh the screen starts meanwhile is already held back.
    day = next;
    previewName = '';
    const mine = ++token;
    render();
    try {
      const [answer, diff] = await Promise.all([
        ApiBinary.one('orgStructureRequest', { at: next }),
        diffDays({ from: today.view.at, to: next }),
      ]);
      if (mine !== token) return;
      const marks = diffMarks(diff.items);
      api.setDecorate((model) => decorateModel(model, marks, 'after'));
      counts = diffCounts(diff.items);
      api.apply({ view: answer.view, unitTypes: answer.unit_types ?? [], myPermissions: answer.my_permissions ?? [] });
      render();
    } catch (err) {
      if (mine !== token) return;
      reset();
      TfToast.show({ tone: 'danger', message: t('asof_load_failed', { date: formatDay(next), message: err.message || '' }) });
    }
  }

  async function applyPreview(set) {
    day = set.effective_date;
    previewName = set.name;
    const mine = ++token;
    render();
    try {
      const preview = await previewChangeSet(set.id);
      if (mine !== token) return;
      if (!preview.preview) throw new Error(preview.error?.message ?? '');
      const marks = diffMarks(preview.items);
      api.setDecorate((model) => decorateModel(model, marks, 'after'));
      counts = diffCounts(preview.items);
      day = preview.at || set.effective_date;
      api.apply({ view: preview.preview, unitTypes: today.unitTypes, myPermissions: today.myPermissions });
      render();
    } catch (err) {
      if (mine !== token) return;
      reset();
      TfToast.show({ tone: 'danger', message: t('asof_load_failed', { date: formatDay(set.effective_date), message: err.message || '' }) });
    }
  }

  function pickDay(anchor) {
    const input = document.createElement('tf-date-field');
    input.setAttribute('label', t('asof_window_field'));
    input.setAttribute('value', day ?? today.view.at);
    openFormWindow({
      title: t('asof_window_title'),
      icon: 'calendar',
      sections: [input],
      validate: () => {
        const ok = isIsoDay(String(input.value ?? ''));
        if (ok) input.removeAttribute('error');
        else input.setAttribute('error', t('date_invalid', { format: dateFormatHint() }));
        return ok;
      },
      collect: () => String(input.value ?? ''),
      submitLabel: t('asof_window_submit'),
      anchor,
      async onSubmit(chosen) {
        await apply(chosen);
        return null;
      },
    });
  }

  async function openMenu(anchor) {
    // Read when the menu opens, not when the tab does: the days are only wanted here.
    await Promise.all([loadPlannedDays(), loadOpenSets()]);
    const items = [
      { label: t('asof_menu_today'), icon: 'calendar', run: () => reset() },
      ...plannedDays.slice(0, 5).map((planned) => ({
        label: t('asof_menu_planned', { date: formatDay(planned) }),
        icon: 'history',
        run: () => apply(planned),
      })),
      ...openSets.slice(0, 5).map((set) => ({
        label: t('asof_menu_preview', { name: set.name, date: formatDay(set.effective_date) }),
        icon: 'sitemap',
        run: () => applyPreview(set),
      })),
      { separator: true },
      { label: t('asof_menu_pick'), icon: 'edit', run: () => pickDay(anchor) },
    ];
    openActionMenu(anchor, items, t('asof_menu_subject'));
  }

  function onClick(e) {
    const target = e.target.closest('[data-act]');
    if (!target) return;
    if (target.dataset.act === 'asof') openMenu(target);
    else if (target.dataset.act === 'asof-back') reset();
  }
  host.addEventListener('click', onClick);
  note.addEventListener('click', onClick);
  const onRequest = (next) => apply(next);
  listeners.add(onRequest);

  render();
  if (requested) {
    const pending = requested;
    requested = null;
    apply(pending);
  }

  return {
    active: () => day !== null,
    /** The screen read today's structure again: keep it for "back to today"; draw it unless another day is on show. */
    setBase(next) {
      today = next;
    },
    reset,
    dispose() {
      listeners.delete(onRequest);
      host.removeEventListener('click', onClick);
      note.removeEventListener('click', onClick);
      host.replaceChildren();
      note.replaceChildren();
    },
  };
}
