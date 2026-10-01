// =============================================================================
// File: modules/org-structure/edit-mode.js
// Description: The edit mode of the Drzewo tab ("Edytuj strukturę", mockup F02),
//   plugged in through `registerEditEntry` — it exists only for `org.admin`.
//   Changes are a DRAFT: every action adds operations to it (edit-draft.js), the
//   canvas draws the structure the draft would leave (a dry run answers with it),
//   and "Zapisz zmiany" saves the whole draft in one atomic batch. Around the
//   canvas: a mode bar with the effective day and "Stan na dziś / Stan na …",
//   undo and redo of the draft, the palette (position, unit, template), the
//   inspector in place of the read-only panel, a strip of people without a
//   position to drag onto vacancies, and the list of pending changes with
//   "Cofnij" on each row and the rule that refused a row named on it.
//
//   A future day is a planned reorganization: the draft is saved as a change set
//   of the Historia tab (history-api.js) — or as batch operations dated that day —
//   and Historia can hand a stored reorganization back to be edited here
//   (history-bridge.js). Leaving the mode, the screen or the page with unsaved
//   changes asks first.
//   The tree tab owns the chart and the model; this module is given a small `api`
//   for them and never reaches into the tab's state.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { TfToast } from '/js/components/tf-toast.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-segmented.js';
import { openConfirmWindow, openEditWindow } from '/js/lib/actions/index.js';
import { dateFormatHint, formatDay, isIsoDay } from '/js/lib/date-format.js';
import { warningText } from '/js/modules/org-structure/model.js';
import { opSummary } from '/js/modules/org-structure/history-model.js';
import { notifyChangeSetsChanged, registerChangeSetEditor, showHistoryTab, showTreeTab } from '/js/modules/org-structure/history-bridge.js';
import { saveChangeSet } from '/js/modules/org-structure/history-api.js';
import { openChangeSetInTree, registerEditEntry } from '/js/modules/org-structure/tree-tab.js';
import { BACKDATED, createDraft, OrgWriteError } from '/js/modules/org-structure/edit-draft.js';
import { createActions } from '/js/modules/org-structure/edit-actions.js';
import { bindInspector, inspectorHtml } from '/js/modules/org-structure/edit-inspector.js';
import { attachPersonDrag, normalizeRoles, normalizeUsers, renderStrip } from '/js/modules/org-structure/edit-people.js';
import { peopleWithoutPosition, reparentImpact } from '/js/modules/org-structure/edit-rules.js';

const t = (key, params) => I18n.t(`org_structure.edit.${key}`, params);
const base = (key, params) => I18n.t(`org_structure.${key}`, params);
const historyT = (key, params) => I18n.t(`org_structure.history.${key}`, params);

const isTyping = (el) => Boolean(el?.closest?.('input, textarea, select, [contenteditable], tf-input, tf-searchbox, tf-textarea'));

/**
 * @param {object} api given by the tree tab: `chart`, `detail`, `top`, `below`, `data()` → `{ view, model, unitTypes }`,
 *   `selection()`, `select(id, kind)`, `clearSelection()`, `apply(answer)`, `me`, `today`, `exit(answer)`,
 *   `renderActions()`, `notifyChanged()`, `fitStage()`
 * @param {{ one: Function, action: Function }} [transport]
 * @param {{ id?: string|null, name?: string, effectiveDate?: string, ops?: Array } | null} [preset] a stored
 *   reorganization to edit
 */
export function openEditMode(api, transport = ApiBinary, preset = null) {
  const { chart } = api;
  const ed = {
    at: preset?.effectiveDate ?? api.today,
    people: [],
    roles: [],
    live: null,
    showLive: false,
    activity: null,
    inspected: null,
    changeSet: preset ? { id: preset.id ?? null, name: preset.name ?? '' } : null,
    saving: false,
    compare: false,
    drawn: null,
  };
  const disposers = [];
  const unitTypes = () => api.data().unitTypes;

  // ---- the draft and its actions ------------------------------------------------

  let actions = null;
  const draft = createDraft({
    send: (kind, payload) => transport.one(kind, payload),
    getLive: () => ed.live,
    askBackdated: (day) => actions.askBackdated(day),
    day: ed.at,
  });

  actions = createActions({
    draft,
    getView: () => api.data().view,
    getModel: () => api.data().model,
    getAt: () => ed.at,
    people: () => ed.people,
    roles: () => ed.roles,
    unitTypes,
    select: (id, kind) => api.select(id, kind),
  });

  const errorText = (err) => actions.errorText(err);

  // What the canvas draws: the draft's preview, or — while "Stan na dziś" is chosen — the structure without it.
  function drawCanvas() {
    const view = ed.showLive ? ed.today : draft.preview?.view ?? ed.live;
    if (!view || view === ed.drawn) return;
    ed.drawn = view;
    api.apply({ view, unitTypes: unitTypes(), myPermissions: ['org.admin'] });
  }

  async function readLive() {
    const body = await transport.one('orgStructureRequest', { at: ed.at });
    ed.live = body.view;
    if (ed.at === api.today) ed.today = body.view;
  }

  async function readToday() {
    ed.today = ed.at === api.today ? ed.live : (await transport.one('orgStructureRequest', {})).view;
  }

  // ---- chrome -----------------------------------------------------------------

  const dayChip = () => {
    if (ed.at > api.today) return `<tf-chip status="info" icon="calendar">${escapeHtml(t('planned_chip'))}</tf-chip>`;
    if (ed.at < api.today) return `<tf-chip status="warn" icon="history">${escapeHtml(t('backdated_chip'))}</tf-chip>`;
    return '';
  };

  const draftDayLabel = () => (ed.at === api.today ? t('view_draft') : t('view_on', { date: formatDay(ed.at) }));

  function topHtml() {
    ed.compare = draft.dirty || ed.at !== api.today;
    const toggle = ed.compare
      ? `<tf-segmented id="org-edit-view" size="sm" value="${ed.showLive ? 'today' : 'draft'}" aria-label="${escapeAttr(t('view_label'))}">`
        + `<option value="today">${escapeHtml(t('view_today'))}</option><option value="draft">${escapeHtml(draftDayLabel())}</option></tf-segmented>`
      : '';
    return `
      <div class="org-edit-bar" role="region" aria-label="${escapeAttr(t('mode_label'))}">
        <tf-chip status="warn" dot title="${escapeAttr(t('mode_hint'))}">${escapeHtml(t('mode_label'))}</tf-chip>
        ${api.me?.name && ed.at === api.today ? `<tf-chip variant="outline" icon="user">${escapeHtml(t('mode_admin', { name: api.me.name }))}</tf-chip>` : ''}
        ${ed.changeSet ? `<tf-chip variant="outline" status="accent" icon="history">${escapeHtml(ed.changeSet.name || t('change_set_new'))}</tf-chip>` : ''}
        <span class="org-edit-date">
          <span>${escapeHtml(t('effective_from'))}</span>
          <tf-date-field id="org-edit-date" value="${escapeAttr(ed.at)}" aria-label="${escapeAttr(t('effective_from'))}"></tf-date-field>
        </span>
        ${ed.at !== api.today ? `<tf-button variant="ghost" size="sm" icon="rotate" data-act="edit-today" title="${escapeAttr(t('back_to_today'))}" aria-label="${escapeAttr(t('back_to_today'))}"></tf-button>` : ''}
        <span id="org-edit-day-chip">${dayChip()}</span>
        <span class="tf-toolbar-spacer"></span>
        ${toggle}
      </div>
      <div class="org-edit-tools" role="toolbar" aria-label="${escapeAttr(t('tools_label'))}">
        <tf-button id="org-edit-undo" variant="ghost" icon="arrow-left" data-act="edit-undo" aria-label="${escapeAttr(t('undo'))}"></tf-button>
        <tf-button id="org-edit-redo" variant="ghost" icon="arrow" data-act="edit-redo" aria-label="${escapeAttr(t('redo'))}"></tf-button>
        <span class="org-edit-sep" aria-hidden="true"></span>
        <tf-button variant="secondary" icon="plus" data-act="edit-add-position">${escapeHtml(t('add_position'))}</tf-button>
        <tf-button variant="secondary" icon="layers" data-act="edit-add-unit">${escapeHtml(t('add_unit'))}</tf-button>
        <tf-button variant="secondary" icon="sparkle" data-act="edit-template">${escapeHtml(t('load_template'))}</tf-button>
        <span class="tf-toolbar-spacer"></span>
        <tf-chip id="org-edit-draft-chip" variant="outline" role="button" tabindex="0" data-act="edit-panel"></tf-chip>
        <tf-chip id="org-edit-warn-chip" variant="outline" icon="alert" role="button" tabindex="0" data-act="edit-panel"></tf-chip>
      </div>`;
  }

  const peopleRows = () => peopleWithoutPosition(ed.people, api.data().view);

  function renderPeople() {
    const host = api.below.querySelector('#org-edit-people');
    if (!host) return;
    renderStrip(host, peopleRows(), {
      title: (count) => t('people_title', { count }),
      hint: `${t('mode_hint')} ${t('people_hint')}`,
      chipHint: t('people_chip_hint'),
      empty: t('people_empty'),
    });
  }

  // ---- the list of pending changes --------------------------------------------------

  // An operation named the way the Historia tab names one, with its ids turned into what the preview shows.
  function describeOp(op) {
    return opSummary(op, draft.preview?.view ?? api.data().view, historyT);
  }

  function renderPanel() {
    const panel = ed.activity?.querySelector('.org-edit-panel');
    if (!panel) return;
    const failed = new Map(draft.errors().map((e) => [e.index, e.error]));
    const rows = draft.ops.length
      ? draft.ops.map((op, index) => (
        `<li class="org-edit-draft-row${failed.has(index) ? ' is-refused' : ''}"><div class="org-edit-draft-text"><span>${escapeHtml(describeOp(op))}</span>`
        + `${failed.has(index) ? `<span class="org-edit-draft-error">${escapeHtml(errorText(failed.get(index)))}</span>` : ''}</div>`
        + `<tf-button variant="ghost" size="sm" data-act="edit-remove" data-index="${index}">${escapeHtml(t('remove_row'))}</tf-button></li>`
      )).join('')
      : `<li class="org-edit-log-empty">${escapeHtml(t('draft_empty'))}</li>`;
    const view = draft.preview?.view ?? api.data().view;
    const warnings = (draft.preview?.warnings ?? view.warnings ?? []);
    const warned = warnings.length
      ? warnings.map((w) => `<li>${escapeHtml(warningText(w, view, base))}</li>`).join('')
      : `<li class="org-edit-log-empty">${escapeHtml(t('warnings_empty'))}</li>`;
    panel.innerHTML = `
      <section><h4 class="org-insp-title">${escapeHtml(t('draft_title', { date: formatDay(ed.at) }))}</h4><ul class="org-edit-log org-edit-draft">${rows}</ul>
        ${draft.dirty ? `<div class="org-insp-actions"><tf-button variant="ghost" size="sm" icon="trash" data-act="edit-discard">${escapeHtml(t('discard'))}</tf-button></div>` : ''}</section>
      <section><h4 class="org-insp-title">${escapeHtml(t('warnings_title'))}</h4><ul class="org-edit-log">${warned}</ul></section>`;
  }

  function openActivity() {
    if (ed.activity) {
      ed.activity.close(true);
      return;
    }
    const win = document.createElement('tf-window');
    win.setAttribute('title', t('activity_title'));
    win.setAttribute('icon', 'history');
    win.setAttribute('buttons', 'close');
    win.setAttribute('draggable', '');
    win.setAttribute('width', '460');
    win.setAttribute('min-width', '320');
    // Low on the left of the chart: away from the inspector on the right and from the toasts in the corner.
    const box = chart.getBoundingClientRect();
    win.setAttribute('initial-x', String(Math.round(Math.max(16, box.left + 16))));
    win.setAttribute('initial-y', String(Math.round(Math.max(16, box.bottom - 400))));
    win.innerHTML = '<div slot="body" class="org-edit-panel"></div>';
    win.addEventListener('closed', () => { if (ed.activity === win) ed.activity = null; });
    win.addEventListener('click', (e) => onPanelClick(e));
    document.body.appendChild(win);
    ed.activity = win;
    renderPanel();
  }

  function onPanelClick(e) {
    const target = e.target.closest?.('[data-act]');
    if (!target) return;
    if (target.dataset.act === 'edit-remove') removeRow(Number(target.dataset.index));
    else if (target.dataset.act === 'edit-discard') discard();
  }

  function removeRow(index) {
    const dropped = draft.remove(index);
    if (dropped > 1) TfToast.show({ tone: 'info', message: t('removed_with_dependents', { count: dropped - 1 }), duration: 6000 });
  }

  // ---- tools, inspector, chrome ------------------------------------------------------------

  function renderTools() {
    const busy = ed.saving;
    const live = ed.showLive;
    const undo = api.top.querySelector('#org-edit-undo');
    const redo = api.top.querySelector('#org-edit-redo');
    if (undo) {
      undo.toggleAttribute('disabled', busy || live || !draft.canUndo);
      undo.setAttribute('title', draft.canUndo ? t('undo') : t('undo_nothing'));
    }
    if (redo) {
      redo.toggleAttribute('disabled', busy || live || !draft.canRedo);
      redo.setAttribute('title', draft.canRedo ? t('redo') : t('redo_nothing'));
    }
    for (const button of api.top.querySelectorAll('[data-act^="edit-add"], [data-act="edit-template"]')) button.toggleAttribute('disabled', busy || live);
    const count = draft.ops.length;
    const errors = draft.errors().length;
    const chip = api.top.querySelector('#org-edit-draft-chip');
    if (chip) {
      chip.setAttribute('label', errors ? t('draft_chip_errors', { count, errors }) : t('draft_chip', { count }));
      chip.setAttribute('status', errors ? 'err' : count ? 'warn' : 'ok');
      chip.setAttribute('icon', errors ? 'alert' : 'history');
    }
    const warn = api.top.querySelector('#org-edit-warn-chip');
    if (warn) {
      const warnings = (draft.preview?.warnings ?? api.data().view.warnings ?? []).length;
      warn.setAttribute('label', t('warnings_chip', { count: warnings }));
      warn.setAttribute('status', warnings ? 'warn' : 'ok');
    }
    api.detail.toggleAttribute('inert', busy);
    api.detail.setAttribute('aria-busy', String(busy));
    chart.editing = !live;
  }

  // The rules the last dry run refused for a card: the operations that name it, and the unit it belongs to.
  function errorsFor(selection) {
    const model = api.data().model;
    const ids = new Set([selection.id]);
    if (selection.kind === 'position') {
      const node = model.nodes.find((n) => n.id === selection.id);
      if (node) ids.add(node.unitId);
    }
    return draft.errors()
      .filter(({ op }) => op && ['unitId', 'positionId', 'assignmentId', 'parentUnitId', 'parentPositionId', 'newParentUnitId', 'newParentPositionId', 'headPositionId']
        .some((field) => typeof op[field] === 'string' && ids.has(op[field])))
      .map(({ error }) => errorText(error));
  }

  function renderInspector() {
    const selection = api.selection();
    const panel = api.detail;
    if (!selection || ed.showLive) {
      panel.hidden = true;
      panel.innerHTML = '';
      ed.inspected = null;
      return;
    }
    const { view, model } = api.data();
    const html = inspectorHtml({
      view, model, selection, unitTypes: unitTypes(), roles: ed.roles, at: ed.at, today: api.today,
      impact: selection.kind === 'position' ? reparentImpact(model, selection.id) : null,
      errors: errorsFor(selection),
    });
    // The same card keeps its scroll through a refresh; another card starts from the top.
    const key = `${selection.kind}:${selection.id}`;
    const scroll = ed.inspected === key ? panel.scrollTop : 0;
    ed.inspected = key;
    panel.innerHTML = html;
    panel.hidden = html === '';
    panel.scrollTop = scroll;
  }

  function renderChrome() {
    renderTools();
    renderPeople();
    renderPanel();
    renderInspector();
    api.renderActions();
    api.fitStage();
  }

  function renderDayBar() {
    api.top.innerHTML = topHtml();
    renderTools();
    api.fitStage();
  }

  // Every change of the draft: the canvas follows its preview, and what hangs off the preview is drawn again.
  disposers.push(draft.subscribe(() => {
    if (draft.preview || !draft.dirty) drawCanvas();
    // The "Stan na" switch exists once there is something to compare the structure with.
    const compare = draft.dirty || ed.at !== api.today;
    if (compare !== ed.compare) {
      ed.compare = compare;
      renderDayBar();
    }
    renderChrome();
  }));

  // ---- behaviour ----------------------------------------------------------------

  const unitOfSelection = () => {
    const selection = api.selection();
    if (!selection) return null;
    if (selection.kind === 'unit') return selection.id;
    return api.data().model.nodes.find((n) => n.id === selection.id)?.unitId ?? null;
  };

  // A refused or cancelled change leaves the inspector showing what the user typed; draw it from the data again.
  const settle = (win) => {
    if (win) win.addEventListener('closed', renderInspector);
    else renderInspector();
  };

  const inspector = {
    field(name, value, el) {
      const selection = api.selection();
      const [scope] = name.split('.');
      let done;
      if (name === 'position.parent') {
        settle(actions.reparentPosition(selection.id, value, el));
        return;
      }
      if (name === 'unit.parent') {
        settle(actions.reparentUnit(unitOfSelection(), value, el));
        return;
      }
      if (scope === 'position') done = actions.positionField(selection.id, name, value);
      else if (scope === 'unit') done = actions.unitField(unitOfSelection(), name, value);
      else done = actions.assignmentField(el.closest('[data-assignment]').dataset.assignment, name, value);
      done.then((ok) => { if (!ok) renderInspector(); });
    },
    act(name, dataset, anchor) {
      const selection = api.selection();
      const unitId = unitOfSelection();
      switch (name) {
        case 'close': api.clearSelection(); break;
        case 'assign': actions.assignWindow(selection.id, anchor); break;
        case 'assignment-replace': actions.assignWindow(selection.id, anchor, actions.assignmentOf(dataset.key)); break;
        case 'assignment-end': actions.endAssignmentWindow(actions.assignmentOf(dataset.key), anchor); break;
        case 'unit-end': actions.endUnitWindow(unitId, anchor); break;
        case 'head-pick': actions.headWindow(unitId, anchor); break;
        case 'deputy-add': actions.addDeputyWindow(unitId, anchor); break;
        case 'deputy-remove': actions.removeDeputy(unitId, dataset.pos); break;
        default:
      }
    },
    reorder(from, to) {
      actions.reorderDeputies(unitOfSelection(), from, to);
    },
  };
  bindInspector(api.detail, inspector);

  async function setDay(day) {
    const input = api.top.querySelector('#org-edit-date');
    if (!isIsoDay(day)) {
      input?.setAttribute('error', t('date_invalid', { format: dateFormatHint() }));
      return;
    }
    if (day === ed.at) return;
    ed.at = day;
    ed.showLive = false;
    draft.setDay(day);
    renderDayBar();
    try {
      await readLive();
      await readToday();
      if (day < api.today && draft.dirty && await actions.askBackdated(day)) draft.confirm();
      await draft.flush();
      drawCanvas();
    } catch (err) {
      TfToast.show({ tone: 'danger', message: errorText(err), duration: 7000 });
    }
  }

  // ---- saving --------------------------------------------------------------------------

  async function afterSaved() {
    draft.clear();
    await readLive();
    await readToday();
    ed.showLive = false;
    drawCanvas();
    api.notifyChanged();
    renderDayBar();
    renderChrome();
  }

  async function save() {
    if (ed.saving || !draft.dirty) return;
    if (draft.errors().length) {
      if (!ed.activity) openActivity();
      TfToast.show({ tone: 'danger', message: t('save_blocked', { count: draft.errors().length }), duration: 7000 });
      return;
    }
    ed.saving = true;
    renderChrome();
    try {
      let out = await draft.save();
      const backdated = out.results.some((r) => r.error?.code === BACKDATED);
      if (!out.ok && backdated && !draft.confirmed && await actions.askBackdated(ed.at)) {
        draft.confirm();
        out = await draft.save();
      }
      if (out.ok) {
        const count = draft.ops.length;
        await afterSaved();
        TfToast.show({ tone: 'success', message: t('saved', { count }), duration: 6000 });
      } else {
        if (!ed.activity) openActivity();
        const first = out.error ?? out.results.find((r) => !r.ok)?.error;
        TfToast.show({ tone: 'danger', message: first ? errorText(new OrgWriteError(first)) : t('err_internal'), duration: 8000 });
      }
    } catch (err) {
      TfToast.show({ tone: 'danger', message: errorText(err), duration: 8000 });
    } finally {
      ed.saving = false;
      renderChrome();
    }
  }

  // A future day is a planned reorganization: stored as a change set of the Historia tab, to be submitted and approved there.
  function savePlan() {
    return openEditWindow({
      subject: null,
      title: t('plan_title'),
      submitLabel: t('plan_submit'),
      note: { tone: 'info', text: t('plan_note', { date: formatDay(ed.at) }) },
      fields: [{ key: 'name', label: t('plan_name'), kind: 'text', required: true, value: ed.changeSet?.name ?? '' }],
      errorMessage: errorText,
      async onSubmit({ name }) {
        const answer = await saveChangeSet({ id: ed.changeSet?.id ?? null, name, effectiveDate: ed.at, ops: draft.ops });
        if (!answer.ok) throw new OrgWriteError(answer.error);
        notifyChangeSetsChanged();
        TfToast.show({ tone: 'success', message: t(answer.valid ? 'plan_saved' : 'plan_saved_invalid', { name }), duration: 8000 });
        draft.clear();
        await closeMode();
        showHistoryTab();
      },
    });
  }

  function discard() {
    if (!draft.dirty) return null;
    return openConfirmWindow({
      kind: 'delete',
      title: t('discard_title'),
      submitLabel: t('discard'),
      subject: t('draft_title', { date: formatDay(ed.at) }),
      consequence: t('discard_consequence', { count: draft.ops.length }),
      onSubmit: async () => { draft.discard(); },
    });
  }

  /** Asks before unsaved changes are given up; resolves whether they may go. */
  function confirmLeave() {
    if (!draft.dirty) return Promise.resolve(true);
    return new Promise((resolve) => {
      let leave = false;
      const win = openConfirmWindow({
        kind: 'delete',
        title: t('leave_title'),
        submitLabel: t('leave_submit'),
        subject: t('draft_title', { date: formatDay(ed.at) }),
        consequence: t('leave_consequence', { count: draft.ops.length }),
        onSubmit: async () => { leave = true; },
      });
      win.addEventListener('closed', () => resolve(leave));
    });
  }

  // ---- toolbar and keyboard -----------------------------------------------------------------

  function act(name, target) {
    const selection = api.selection();
    switch (name) {
      case 'edit-undo': draft.undo(); break;
      case 'edit-redo': draft.redo(); break;
      case 'edit-today': setDay(api.today); break;
      case 'edit-add-position': {
        const node = selection?.kind === 'position' ? api.data().model.nodes.find((n) => n.id === selection.id) : null;
        actions.addPositionWindow({ unitId: unitOfSelection(), parentPositionId: node ? node.id : null }, target);
        break;
      }
      case 'edit-add-unit': actions.addUnitWindow(unitOfSelection(), target); break;
      case 'edit-template': actions.templateWindow(target); break;
      case 'edit-panel': openActivity(); break;
      case 'edit-save': save(); break;
      case 'edit-save-plan': savePlan(); break;
      case 'edit-discard': discard(); break;
      case 'edit-exit': closeMode(); break;
      default:
    }
  }

  function onTopClick(e) {
    const target = e.target.closest('[data-act]');
    if (!target || target.hasAttribute('disabled')) return;
    act(target.dataset.act, target);
  }

  function onTopKey(e) {
    if ((e.key === 'Enter' || e.key === ' ') && e.target.closest?.('tf-chip[data-act]')) {
      e.preventDefault();
      e.target.closest('tf-chip[data-act]').click();
    }
  }

  function onTopChange(e) {
    if (e.target.id === 'org-edit-date') {
      setDay(String(e.detail?.value ?? ''));
    } else if (e.target.id === 'org-edit-view') {
      ed.showLive = e.detail?.value === 'today';
      drawCanvas();
      renderChrome();
    }
  }

  // The menu opens under a stand-in element: the chart redraws its cards, so a handle inside it would not survive.
  function anchorAt(rect) {
    const anchor = document.createElement('span');
    anchor.className = 'org-menu-anchor';
    Object.assign(anchor.style, {
      position: 'fixed', left: `${rect.left}px`, top: `${rect.top}px`, width: `${Math.max(rect.width, 1)}px`, height: `${Math.max(rect.height, 1)}px`,
    });
    anchor.focus = () => chart.querySelector('.tf-orgtree__svg')?.focus({ preventScroll: true });
    document.body.appendChild(anchor);
    return anchor;
  }

  function onNodeMenu(e) {
    const { id, kind, rect } = e.detail;
    const anchor = anchorAt(rect);
    const { model } = api.data();
    let menu = null;
    if (kind === 'unit') menu = actions.unitMenu(model.units.find((u) => u.id === id), anchor);
    else menu = actions.positionMenu(model.nodes.find((n) => n.id === id), anchor);
    if (menu) menu.addEventListener('close', () => anchor.remove());
    else anchor.remove();
  }

  function onNodeOpen() {
    api.detail.querySelector('[data-edit]')?.focus();
  }

  function onNodeDrop(e) {
    const { source, target } = e.detail;
    if (source.kind === 'unit') actions.reparentUnit(source.id, target.id);
    else actions.reparentPosition(source.id, target.id);
  }

  function onDocumentKey(e) {
    if (!(e.ctrlKey || e.metaKey) || e.altKey || isTyping(e.target) || ed.saving) return;
    const key = e.key.toLowerCase();
    if (key === 'z') {
      e.preventDefault();
      if (e.shiftKey) draft.redo();
      else draft.undo();
    } else if (key === 'y') {
      e.preventDefault();
      draft.redo();
    }
  }

  // Closing the tab or the browser with a draft loses it; the browser's own question is the only one it allows.
  const onBeforeUnload = (e) => {
    if (!draft.dirty) return;
    e.preventDefault();
    e.returnValue = '';
  };

  // ---- the header's buttons (drawn by the tree tab) ------------------------------------------------

  function actionsHtml() {
    const dirty = draft.dirty;
    const future = ed.at > api.today;
    const busy = ed.saving ? ' disabled' : '';
    const save = `<tf-button id="org-edit-save" variant="primary" icon="save" data-act="edit-save"${dirty && !busy ? '' : ' disabled'}>${escapeHtml(t('save'))}</tf-button>`;
    const plan = `<tf-button id="org-edit-save-plan" variant="primary" icon="calendar" data-act="edit-save-plan"${dirty && !busy ? '' : ' disabled'}>${escapeHtml(ed.changeSet?.id ? t('plan_update') : t('plan_save'))}</tf-button>`;
    // A planned day is a reorganization (approved by a second administrator in Historia), never a direct write.
    const saves = future ? plan : save;
    return `
      <tf-button id="org-tree-export" variant="secondary" icon="download" data-act="export">${escapeHtml(base('export'))}</tf-button>
      <tf-button id="org-edit-exit" variant="secondary" icon="x" data-act="edit-exit"${busy}>${escapeHtml(t('exit'))}</tf-button>
      ${saves}`;
  }

  // ---- open --------------------------------------------------------------------------------------

  chart.editing = true;
  chart.dropRule = (source, target) => actions.dropRule()(source, target);
  api.top.hidden = false;
  api.below.hidden = false;
  api.detail.classList.add('org-detail--edit');
  api.below.innerHTML = '<div id="org-edit-people" class="org-people"></div>';
  api.top.addEventListener('click', onTopClick);
  api.top.addEventListener('keydown', onTopKey);
  api.top.addEventListener('change', onTopChange);
  chart.addEventListener('node-menu', onNodeMenu);
  chart.addEventListener('node-drop', onNodeDrop);
  chart.addEventListener('node-open', onNodeOpen);
  document.addEventListener('keydown', onDocumentKey);
  window.addEventListener('beforeunload', onBeforeUnload);
  disposers.push(attachPersonDrag(api.below, {
    chart,
    model: () => api.data().model,
    name: (id) => ed.people.find((p) => p.id === id)?.name ?? '',
    onDrop: (userId, positionId) => actions.assignDropped(userId, positionId),
    onReject: (reason) => TfToast.show({ tone: 'danger', message: t(`drop_person_${reason}`), duration: 6000 }),
    onPick: (userId, chip) => actions.pickVacancyWindow(userId, chip),
  }));
  ed.live = api.data().view;
  ed.today = ed.live;
  renderDayBar();
  renderChrome();

  // The structure on the effective day (another day than the screen's when a reorganization is opened), then the draft.
  (async () => {
    try {
      if (ed.at !== api.today) {
        await readLive();
        await readToday();
      }
      if (preset?.ops?.length) await draft.load(preset.ops, ed.at);
      drawCanvas();
      renderDayBar();
      renderChrome();
    } catch (err) {
      TfToast.show({ tone: 'danger', message: t('refresh_failed', { message: err.message || '' }), duration: 9000 });
    }
  })();

  // People and roles come from other modules of the platform; the structure is editable without them.
  transport.action('iamListUsersRequest').then((r) => { ed.people = normalizeUsers(r?.users); renderChrome(); }).catch(() => {
    TfToast.show({ tone: 'warning', message: t('people_load_failed'), duration: 6000 });
  });
  transport.action('roleCatalogListRequest', { isActive: true }).then((r) => { ed.roles = normalizeRoles(r?.roles, I18n.getLanguage()); renderInspector(); }).catch(() => {});

  function dispose() {
    chart.editing = false;
    chart.dropRule = null;
    chart.removeEventListener('node-menu', onNodeMenu);
    chart.removeEventListener('node-drop', onNodeDrop);
    chart.removeEventListener('node-open', onNodeOpen);
    document.removeEventListener('keydown', onDocumentKey);
    window.removeEventListener('beforeunload', onBeforeUnload);
    api.top.removeEventListener('click', onTopClick);
    api.top.removeEventListener('keydown', onTopKey);
    api.top.removeEventListener('change', onTopChange);
    for (const stop of disposers) stop();
    ed.activity?.close(true);
    api.top.replaceChildren();
    api.top.hidden = true;
    api.below.replaceChildren();
    api.below.hidden = true;
    api.detail.classList.remove('org-detail--edit');
    api.detail.removeAttribute('inert');
  }

  /** Leaves the mode (asking first when there are unsaved changes): the chart returns to today's structure. */
  async function closeMode() {
    if (!(await confirmLeave())) return false;
    dispose();
    const body = await transport.one('orgStructureRequest', {});
    api.exit({ view: body.view, unitTypes: body.unit_types ?? [], myPermissions: body.my_permissions ?? [] });
    return true;
  }

  return {
    templateWindow: () => actions.templateWindow(null),
    addUnitWindow: () => actions.addUnitWindow(null, null),
    renderInspector,
    actionsHtml,
    act,
    dirty: () => draft.dirty,
    canLeave: confirmLeave,
    dispose,
    close: closeMode,
    /** Replaces the draft with a stored reorganization (after asking when there are unsaved changes). */
    async replaceWith(next) {
      if (!(await confirmLeave())) return false;
      ed.changeSet = { id: next.id ?? null, name: next.name ?? '' };
      ed.at = next.effectiveDate ?? ed.at;
      ed.showLive = false;
      await readLive();
      await readToday();
      await draft.load(next.ops ?? [], ed.at);
      drawCanvas();
      renderDayBar();
      renderChrome();
      return true;
    },
  };
}

registerEditEntry({ open: openEditMode });

// Historia hands a stored reorganization over: the Drzewo tab is shown and the edit mode opens on it.
registerChangeSetEditor({
  async open(request) {
    showTreeTab();
    return openChangeSetInTree(request);
  },
});
