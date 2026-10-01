// =============================================================================
// File: modules/org-structure/history-changeset.js
// Description: The planned reorganizations of the Historia tab (mockup F04,
//   "Reorganizacja planowana"): a card per reorganization with what it would
//   change (a dry run on the server, on today's structure), who prepared it and
//   who has to approve it, and the actions — "Zatwierdź reorganizację" (another
//   administrator than the author), "Wyślij do zatwierdzenia", "Edytuj w trybie
//   edycji" (handed to the edit mode through history-bridge.js, with a basic
//   editor of its own when there is none) and "Wycofaj reorganizację".
//   Only for `org.admin`: a plan names people and moves before anyone is told.
//   Every refusal is a typed error the window shows in place; nothing here
//   decides who may approve — the server does, and the disabled button only says why.
// =============================================================================

import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { TfToast } from '/js/components/tf-toast.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-spinner.js';
import { openConfirmWindow } from '/js/lib/actions/index.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import { addDays, formatDay, isIsoDay } from '/js/lib/date-format.js';
import {
  changeSetFlags, diffLine, errorText, failedOps, groupChangeSets, opSummary, withoutOp,
} from '/js/modules/org-structure/history-model.js';
import {
  approveChangeSet, getChangeSet, listChangeSets, previewChangeSet, saveChangeSet, submitChangeSet, withdrawChangeSet,
} from '/js/modules/org-structure/history-api.js';
import { notifyChangeSetsChanged, openChangeSetInEditor } from '/js/modules/org-structure/history-bridge.js';

const t = (key, params) => I18n.t(`org_structure.history.${key}`, params);

const CHANGES_SHOWN = 6;

const fail = (answer) => new Error(errorText(answer.error, t));

/**
 * @param {object} deps
 * @param {HTMLElement} deps.host the section the panel fills
 * @param {() => { today: string, meHex: string, view: object, unitTypes: Array }} deps.context today's day, the session user, today's structure and the unit types
 * @param {(day: string, options?: { keepComparison?: boolean }) => void} deps.onShowDay shows the structure on a day
 * @param {(comparison: object) => void} deps.onCompare draws a comparison in "Przed i po"
 * @param {() => void} deps.onChanged a reorganization was approved or withdrawn: the screen reads the structure again
 * @param {(sets: Array) => void} [deps.onLoaded] the list was read: the days of the open ones are marks on the date jumper
 * @param {() => boolean} [deps.isComparing] something is already drawn in "Przed i po"; the soonest open reorganization
 *   is drawn there by itself only while nothing is
 */
export function createReorgPanel({
  host, context, onShowDay, onCompare, onChanged, onLoaded = () => {}, isComparing = () => true,
}) {
  let sets = [];
  let previews = new Map();
  let showClosed = false;
  let loadError = '';
  let soleAdmin = false;
  let disposed = false;
  let autoCompared = false;

  // ---- data -------------------------------------------------------------------

  // A preview is a dry run of the whole reorganization on the write connection: kept until the reorganization
  // changes, not redone each time the tab is shown.
  const keyOf = (set) => [set.state, set.op_count, set.effective_date, set.name, set.author_user_id].join('|');

  async function loadPreview(set) {
    const key = keyOf(set);
    previews.set(set.id, { key, status: 'loading' });
    try {
      const preview = await previewChangeSet(set.id);
      previews.set(set.id, { key, status: 'ready', preview });
    } catch (err) {
      previews.set(set.id, { key, status: 'failed', message: err.message || '' });
    }
    if (disposed) return;
    render();
    autoCompare(set);
  }

  // The mockup opens "Przed i po" on the soonest planned reorganization: the page answers "what will change" at once.
  function autoCompare(set) {
    const soonest = sets.filter((s) => s.state === 'draft' || s.state === 'pending')
      .sort((a, b) => a.effective_date.localeCompare(b.effective_date))[0];
    const comparison = soonest?.id === set.id ? comparisonOf(set) : null;
    if (!comparison || autoCompared || isComparing()) return;
    autoCompared = true;
    onCompare(comparison);
  }

  function comparisonOf(set) {
    const entry = previews.get(set.id);
    if (entry?.status !== 'ready' || !entry.preview.live || !entry.preview.preview) return null;
    const { live, preview, items } = entry.preview;
    return {
      before: { view: live, label: t('compare_before', { date: formatDay(context().today) }) },
      after: { view: preview, label: t('compare_after_plan', { date: formatDay(preview.at) }), planned: true },
      items,
      unitTypes: context().unitTypes,
    };
  }

  async function reload() {
    try {
      const { items, soleAdmin: sole } = await listChangeSets();
      sets = items;
      soleAdmin = sole;
      loadError = '';
    } catch (err) {
      sets = [];
      loadError = err.message || '';
    }
    const live = new Map(sets.map((s) => [s.id, s]));
    previews = new Map([...previews].filter(([id, entry]) => live.has(id) && entry.key === keyOf(live.get(id))));
    render();
    onLoaded(sets);
    for (const set of sets.filter((s) => s.state === 'draft' || s.state === 'pending')) {
      if (!previews.has(set.id)) loadPreview(set);
    }
  }

  // ---- markup -----------------------------------------------------------------


  function changesHtml(set) {
    const entry = previews.get(set.id);
    if (!entry || entry.status === 'loading') {
      return `<div class="org-reorg-note"><tf-spinner size="sm"></tf-spinner> ${escapeHtml(t('changes_loading'))}</div>`;
    }
    if (entry.status === 'failed') {
      return `<div class="org-reorg-note is-error">${escapeHtml(t('changes_failed', { message: entry.message }))}</div>`;
    }
    const { items, results } = entry.preview;
    const failed = failedOps(results, t);
    const shown = items.slice(0, CHANGES_SHOWN).map((item, index) => {
      const line = diffLine(item, t);
      const diff = line.before === undefined
        ? ''
        : `<span class="s"><span class="o">${escapeHtml(line.before)}</span> → <span class="n">${escapeHtml(line.after)}</span></span>`;
      return `<li class="org-reorg-change"><span class="num">${index + 1}</span><div><b>${escapeHtml(line.text)}</b>${diff}</div></li>`;
    }).join('');
    const more = items.length > CHANGES_SHOWN
      ? `<li class="org-reorg-more">${escapeHtml(t('changes_more', { count: items.length - CHANGES_SHOWN }))}</li>` : '';
    const empty = items.length === 0 && failed.length === 0
      ? `<li class="org-reorg-more">${escapeHtml(t('changes_none'))}</li>` : '';
    const invalid = failed.length
      ? `<tf-alert tone="warning" message="${escapeAttr(t('invalid_ops', { count: failed.length }))}"></tf-alert>
         <ul class="org-reorg-failed">${failed.slice(0, 4).map((f) => `<li>${escapeHtml(t('invalid_op_line', { index: f.index + 1, text: f.text }))}</li>`).join('')}</ul>`
      : '';
    return `<ol class="org-reorg-changes">${shown}${more}${empty}</ol>${invalid}`;
  }

  function approvalHtml(set) {
    const author = `<div class="org-reorg-row"><span>${escapeHtml(t('author', { name: set.author_name || '—' }))}</span></div>`;
    if (set.state === 'applied') {
      return `${author}<div class="org-reorg-row"><span>${escapeHtml(t('approver', { name: set.approver_name || '—' }))}</span><tf-chip status="ok" icon="check">${escapeHtml(t('state.applied'))}</tf-chip></div>`;
    }
    if (set.state === 'pending') {
      return `${author}<div class="org-reorg-row"><span>${escapeHtml(soleAdmin ? t('sole_admin_note') : t('appr_pending'))}</span><tf-chip status="warn" dot>${escapeHtml(t('state.pending'))}</tf-chip></div>`;
    }
    if (set.state === 'draft') {
      return `${author}<div class="org-reorg-row"><span>${escapeHtml(t('appr_draft'))}</span><tf-chip status="neutral">${escapeHtml(t('state.draft'))}</tf-chip></div>`;
    }
    return `${author}<div class="org-reorg-row"><tf-chip status="neutral">${escapeHtml(t(`state.${set.state}`))}</tf-chip></div>`;
  }

  function actionsHtml(set, flags) {
    const buttons = [];
    if (flags.canApprove) {
      buttons.push(`<tf-button variant="primary" icon="check" data-act="approve">${escapeHtml(t('btn_approve'))}</tf-button>`);
    } else if (flags.approveBlockedByAuthor) {
      buttons.push(`<tf-button variant="primary" icon="check" disabled title="${escapeAttr(t('btn_approve_blocked'))}" data-act="approve">${escapeHtml(t('btn_approve'))}</tf-button>`);
    }
    if (flags.canSubmit) {
      buttons.push(`<tf-button variant="primary" icon="arrow-up" data-act="submit">${escapeHtml(t('btn_submit'))}</tf-button>`);
    }
    if (flags.canEdit) {
      buttons.push(`<tf-button variant="ghost" size="sm" icon="edit" data-act="edit">${escapeHtml(t('btn_edit'))}</tf-button>`);
    }
    if (flags.canWithdraw) {
      buttons.push(`<tf-button variant="ghost" size="sm" icon="trash" data-act="withdraw">${escapeHtml(t('btn_withdraw'))}</tf-button>`);
    }
    buttons.push(`<tf-button variant="ghost" size="sm" icon="calendar" data-act="show-day">${escapeHtml(t('btn_show_day', { date: formatDay(set.effective_date) }))}</tf-button>`);
    if (flags.open) {
      buttons.push(`<tf-button variant="ghost" size="sm" icon="sitemap" data-act="compare">${escapeHtml(t('btn_compare'))}</tf-button>`);
    }
    const blocked = flags.approveBlockedByAuthor
      ? `<div class="org-reorg-note">${escapeHtml(t('btn_approve_blocked'))}</div>`
      : flags.selfApproval ? `<div class="org-reorg-note">${escapeHtml(t('sole_admin_note'))}</div>` : '';
    return `<div class="org-reorg-actions">${buttons.join('')}</div>${blocked}`;
  }

  function cardHtml(set) {
    const { today, meHex } = context();
    const flags = changeSetFlags(set, { me: meHex, today, soleAdmin });
    const date = formatDay(set.effective_date);
    const sub = flags.datePassed && flags.open
      ? t('sub_passed', { date })
      : t('sub', { date, count: set.op_count });
    const note = flags.open
      ? `<div class="org-reorg-calm">${escapeHtml(t('calm_note', { date }))}</div>` : '';
    return `
      <article class="org-reorg${flags.open ? '' : ' is-closed'}" data-id="${escapeAttr(set.id)}">
        <h3 class="org-reorg-title">${escapeHtml(set.name)}</h3>
        <div class="org-reorg-sub">${escapeHtml(sub)}</div>
        ${flags.open ? changesHtml(set) : ''}
        <div class="org-reorg-approval">${approvalHtml(set)}</div>
        ${actionsHtml(set, flags)}
        ${note}
      </article>`;
  }

  function render() {
    if (disposed) return;
    const groups = groupChangeSets(sets, context().today);
    const closed = [...groups.applied, ...groups.withdrawn];
    const parts = [];
    parts.push(`<div class="org-hist-card-head"><div class="org-hist-card-title">${escapeHtml(t('reorg_title'))}</div></div>`);
    if (loadError) parts.push(`<div class="org-reorg-note is-error">${escapeHtml(t('reorg_load_failed', { message: loadError }))}</div>`);
    if (groups.open.length === 0 && !loadError) {
      parts.push(`<div class="org-hist-empty">${escapeHtml(t('reorg_none'))}<br><span class="org-hist-hint">${escapeHtml(t('reorg_none_hint'))}</span></div>`);
    }
    parts.push(...groups.open.map(cardHtml), ...groups.upcoming.map(cardHtml));
    if (closed.length) {
      parts.push(`<tf-button variant="ghost" size="sm" icon="history" data-act="toggle-closed">${escapeHtml(showClosed ? t('closed_hide') : t('closed_show', { count: closed.length }))}</tf-button>`);
      if (showClosed) parts.push(...closed.map(cardHtml));
    }
    host.innerHTML = parts.join('');
  }

  // ---- actions ----------------------------------------------------------------

  const findSet = (id) => sets.find((s) => s.id === id);

  function afterChange() {
    previews.clear();
    notifyChangeSetsChanged();
    return reload();
  }

  function openApprove(set, anchor) {
    const date = formatDay(set.effective_date);
    openFormWindow({
      title: t('approve_title'),
      icon: 'check',
      subject: set.name,
      note: { tone: 'warning', text: t('approve_note', { date }) },
      sections: [],
      collect: () => ({}),
      submitLabel: t('approve_submit'),
      anchor,
      async onSubmit() {
        const answer = await approveChangeSet(set.id);
        if (!answer.ok) {
          // The structure moved on: the card's preview is stale too.
          previews.delete(set.id);
          await reload();
          throw fail(answer);
        }
        await reload();
        onChanged();
        return { message: t('approve_done', { name: set.name, date }) };
      },
    });
  }

  function openWithdraw(set, anchor) {
    openConfirmWindow({
      kind: 'archive',
      title: t('withdraw_title'),
      subject: set.name,
      consequence: t(set.state === 'applied' ? 'withdraw_applied_consequence' : 'withdraw_consequence'),
      submitLabel: t('withdraw_submit'),
      anchor,
      async onSubmit() {
        const answer = await withdrawChangeSet(set.id);
        if (!answer.ok) {
          await reload();
          throw fail(answer);
        }
        await afterChange();
        return { message: t('withdraw_done', { name: set.name }) };
      },
    });
  }

  async function submit(set) {
    try {
      const answer = await submitChangeSet(set.id);
      if (!answer.ok) {
        TfToast.show({ tone: 'danger', message: errorText(answer.error, t), duration: 9000 });
        previews.delete(set.id);
        await reload();
        return;
      }
      TfToast.show({ tone: 'success', message: t('submit_done', { name: set.name }) });
      await afterChange();
    } catch (err) {
      TfToast.show({ tone: 'danger', message: err.message || t('error.unknown') });
    }
  }

  async function compare(set) {
    let entry = previews.get(set.id);
    if (!entry || entry.status !== 'ready') {
      await loadPreview(set);
      entry = previews.get(set.id);
    }
    const comparison = comparisonOf(set);
    if (!comparison) {
      TfToast.show({ tone: 'danger', message: t('changes_failed', { message: entry?.message ?? '' }) });
      return;
    }
    onCompare(comparison);
    onShowDay(comparison.after.view.at, { keepComparison: true });
  }

  // ---- the basic editor, for when the edit mode does not take a reorganization --

  function nameAndDateSections({ name, day, minDay }) {
    const nameInput = document.createElement('tf-input');
    nameInput.setAttribute('label', t('field_name'));
    nameInput.setAttribute('value', name);
    const dayInput = document.createElement('tf-date-field');
    dayInput.setAttribute('label', t('field_date'));
    dayInput.setAttribute('value', day);
    dayInput.setAttribute('min', minDay);
    return { nameInput, dayInput };
  }

  function readInputs({ nameInput, dayInput }) {
    return { name: String(nameInput.value ?? '').trim(), day: String(dayInput.value ?? '') };
  }

  function validInputs(inputs) {
    const { name, day } = readInputs(inputs);
    inputs.nameInput.removeAttribute('error');
    inputs.dayInput.removeAttribute('error');
    let ok = true;
    if (!name) {
      inputs.nameInput.setAttribute('error', t('error.empty_field'));
      ok = false;
    }
    if (!isIsoDay(day)) {
      inputs.dayInput.setAttribute('error', t('error.invalid_date'));
      ok = false;
    }
    return ok;
  }

  function openNew(anchor) {
    const { today } = context();
    const inputs = nameAndDateSections({ name: '', day: addDays(today, 30), minDay: today });
    openFormWindow({
      title: t('new_title'),
      icon: 'plus',
      note: { tone: 'info', text: t('new_hint') },
      sections: [inputs.nameInput, inputs.dayInput],
      validate: () => validInputs(inputs),
      collect: () => readInputs(inputs),
      submitLabel: t('new_submit'),
      anchor,
      async onSubmit({ name, day }) {
        const answer = await saveChangeSet({ name, effectiveDate: day, ops: [] });
        if (!answer.ok || !answer.changeSet) throw fail(answer);
        await afterChange();
        const saved = answer.changeSet;
        const taken = await openChangeSetInEditor({
          id: saved.id, name: saved.name, effectiveDate: saved.effective_date, ops: saved.ops,
        }).catch(() => false);
        return { message: taken ? t('new_done', { name }) : t('new_editor_missing', { name }) };
      },
    });
  }

  async function openEdit(set, anchor) {
    let answer;
    try {
      answer = await getChangeSet(set.id);
    } catch (err) {
      TfToast.show({ tone: 'danger', message: err.message || t('error.unknown') });
      return;
    }
    if (!answer.changeSet) {
      TfToast.show({ tone: 'danger', message: errorText(answer.error, t) });
      await reload();
      return;
    }
    const full = answer.changeSet;
    const taken = await openChangeSetInEditor({
      id: full.id, name: full.name, effectiveDate: full.effective_date, ops: full.ops,
    });
    if (taken) return;
    openBasicEditor(full, anchor);
  }

  // Name, day and the operations that can be dropped. Adding one needs the canvas: it is the edit mode's job.
  function openBasicEditor(full, anchor) {
    const { today, view } = context();
    let ops = full.ops;
    const inputs = nameAndDateSections({ name: full.name, day: full.effective_date, minDay: today });
    const list = document.createElement('div');
    list.className = 'org-reorg-ops';
    const drawOps = () => {
      list.innerHTML = ops.length
        ? ops.map((op, index) => `
            <div class="org-reorg-op">
              <span>${escapeHtml(opSummary(op, view, t))}</span>
              <tf-button variant="ghost" size="sm" icon="x" data-drop="${index}" aria-label="${escapeAttr(t('edit_op_remove'))}"></tf-button>
            </div>`).join('')
        : `<div class="org-reorg-note">${escapeHtml(t('edit_ops_empty'))}</div>`;
    };
    drawOps();
    list.addEventListener('click', (e) => {
      const drop = e.target.closest('[data-drop]');
      if (!drop) return;
      ops = withoutOp(ops, Number(drop.dataset.drop));
      drawOps();
    });
    const heading = document.createElement('div');
    heading.className = 'org-hist-hint';
    heading.textContent = t('edit_ops', { count: ops.length });
    openFormWindow({
      title: t('edit_title'),
      icon: 'edit',
      subject: full.name,
      note: { tone: 'info', text: t('edit_reset_note') },
      sections: [inputs.nameInput, inputs.dayInput, heading, list],
      validate: () => validInputs(inputs),
      collect: () => readInputs(inputs),
      submitLabel: t('edit_submit'),
      anchor,
      async onSubmit({ name, day }) {
        const answer = await saveChangeSet({ id: full.id, name, effectiveDate: day, ops });
        if (!answer.ok) throw fail(answer);
        await afterChange();
        return { message: t('edit_done', { name }) };
      },
    });
  }

  // ---- events -----------------------------------------------------------------

  function onClick(e) {
    const target = e.target.closest('[data-act]');
    if (!target || target.hasAttribute('disabled')) return;
    const card = target.closest('[data-id]');
    const set = card ? findSet(card.dataset.id) : null;
    switch (target.dataset.act) {
      case 'approve': if (set) openApprove(set, target); break;
      case 'withdraw': if (set) openWithdraw(set, target); break;
      case 'submit': if (set) submit(set); break;
      case 'edit': if (set) openEdit(set, target); break;
      case 'show-day': if (set) onShowDay(set.effective_date); break;
      case 'compare': if (set) compare(set); break;
      case 'toggle-closed':
        showClosed = !showClosed;
        render();
        break;
      default: break;
    }
  }
  host.addEventListener('click', onClick);

  render();
  reload();
  return {
    reload,
    render,
    openNew,
    dispose() {
      disposed = true;
      host.removeEventListener('click', onClick);
      host.replaceChildren();
    },
  };
}
