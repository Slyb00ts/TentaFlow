// =============================================================================
// File: modules/org-structure/list-tab.js
// Description: The "Lista i import" tab of the organization structure screen
//   (mockup F03): the people table (person and position, unit, manager, since
//   when), the search box, the filter chips (all, vacancies, changes since a
//   day, people without an account), sorting, the "⋯" menu of a row, "Dodaj
//   stanowisko" and the export / import entries in the screen's header.
//   Everyone in the organization reads the list and exports it (the server
//   decides which columns a member gets); the row menu, "Dodaj stanowisko"
//   and the import exist only for `org.admin` — a control a non-admin could
//   only be refused by is not drawn at all. Writes live in list-actions.js and
//   the import window in import-panel.js.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeAttr, escapeHtml } from '/js/utils.js';
import { TfToast } from '/js/components/tf-toast.js';
import '/js/components/tf-table.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-input.js';
import '/js/components/tf-button.js';
import '/js/components/tf-avatar.js';
import { openActionMenu } from '/js/lib/actions/index.js';
import { focusTarget } from '/js/lib/actions/fields.js';
import { downloadBytes } from '/js/lib/download.js';
import { addDays, formatDay, isIsoDay } from '/js/lib/date-format.js';
import { initialsOf, unitColor } from '/js/modules/org-structure/tree.js';
import {
  CHIPS, DEFAULT_CHANGES_DAYS, chipCounts, filterRows, listRows, menuItems,
} from '/js/modules/org-structure/list-model.js';
import { createListActions } from '/js/modules/org-structure/list-actions.js';
import { accountPeople, createCoverActions } from '/js/modules/org-structure/cover-actions.js';
import { openImportPanel } from '/js/modules/org-structure/import-panel.js';
import { openHandover } from '/js/modules/org-structure/handover-nav.js';

const t = (key, params) => I18n.t(`org_structure.${key}`, params);
const lt = (key, params) => I18n.t(`org_structure.list.${key}`, params);

let state = null;

const isAdmin = () => Boolean(state?.myPermissions.includes('org.admin'));

// ---------------------------------------------------------------------------
// Cells (written into the table's shadow root; styled by org-structure-cells.css)
// ---------------------------------------------------------------------------

function personCell(row) {
  const avatar = row.vacant
    ? '<span class="org-cell-vac" aria-hidden="true"><svg class="icon"><use href="#i-user"/></svg></span>'
    : `<tf-avatar size="sm" initials="${escapeAttr(initialsOf(row.personName))}"></tf-avatar>`;
  return `<span class="org-cell-person${row.vacant ? ' is-vacant' : ''}">${avatar}`
    + `<span class="org-cell-text"><span class="org-cell-name">${escapeHtml(row.personName)}</span>`
    + `<span class="org-cell-role">${escapeHtml(row.positionName)}</span></span></span>`;
}

function unitCell(row) {
  const unit = state.units.get(row.unitId);
  const color = unit ? unitColor(unit) : 'var(--tf-text-3)';
  return `<span class="org-cell-unit"><i class="org-cell-dot" style="background:${escapeAttr(color)}"></i>${escapeHtml(row.unitName)}</span>`;
}

function tableRow(row) {
  return {
    _id: row._id,
    person: { display: personCell(row), value: row.personName },
    unit: { display: unitCell(row), value: row.unitName },
    reportsTo: row.reportsTo || '—',
    since: { display: escapeHtml(formatDay(row.since)), value: row.since },
  };
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

function shell() {
  const admin = isAdmin();
  const hasUnits = state.view.units.length > 0;
  return `
    <div class="org-list-tools">
      <tf-searchbox class="org-list-search" placeholder="${escapeAttr(lt('search_placeholder'))}"
        aria-label="${escapeAttr(lt('search_label'))}"></tf-searchbox>
      <tf-filter-chips class="org-list-chips" data-role="chips"></tf-filter-chips>
      <tf-date-field class="org-list-since" data-role="since" label="${escapeAttr(lt('since_label'))}" hidden></tf-date-field>
      <div class="org-list-tools-end">
        <tf-button variant="secondary" icon="download" data-act="export">${escapeHtml(lt('export_button'))}</tf-button>
        ${admin ? `<tf-button variant="secondary" icon="plus" data-act="add"${hasUnits ? '' : ` disabled title="${escapeAttr(lt('add_no_units'))}"`}>${escapeHtml(lt('add_position'))}</tf-button>` : ''}
      </div>
    </div>
    <tf-table class="org-list-table" data-role="table" sortable empty-message="${escapeAttr(lt('empty_filtered'))}">
      <tf-column key="person" label="${escapeAttr(lt('col_person'))}" renderer="html" sortable fill></tf-column>
      <tf-column key="unit" label="${escapeAttr(t('col_unit'))}" renderer="html" sortable></tf-column>
      <tf-column key="reportsTo" label="${escapeAttr(t('col_reports_to'))}" sortable></tf-column>
      <tf-column key="since" label="${escapeAttr(lt('col_since'))}" renderer="html" sortable></tf-column>
    </tf-table>
    <div class="org-table-footer" data-role="footer"></div>`;
}

function renderHeaderActions() {
  const host = byId('org-list-actions');
  if (!host) return;
  host.addEventListener('click', onClick);
  host.innerHTML = `
    <tf-button variant="secondary" icon="download" data-act="export">${escapeHtml(t('export'))}</tf-button>
    ${isAdmin() ? `<tf-button variant="primary" icon="arrow-up" data-act="import">${escapeHtml(lt('import_button'))}</tf-button>` : ''}`;
}

function chipFilters(counts) {
  const label = (id) => (id === 'changes'
    ? lt('chip_changes', { date: formatDay(state.since) })
    : lt(`chip_${id}`));
  return CHIPS.map((id) => ({ id, label: escapeHtml(label(id)), count: counts[id], active: id === state.chip }));
}

function applyFilters() {
  const counts = chipCounts(state.rows, state.since);
  const shown = filterRows(state.rows, { chip: state.chip, query: state.query, since: state.since });
  const chips = state.host.querySelector('[data-role="chips"]');
  chips.filters = chipFilters(counts);
  state.host.querySelector('[data-role="since"]').hidden = state.chip !== 'changes';
  const table = state.host.querySelector('[data-role="table"]');
  table.rows = shown.map(tableRow);
  state.shown = new Map(shown.map((r) => [r._id, r]));
  state.host.querySelector('[data-role="footer"]').textContent = state.rows.length
    ? lt('showing', { shown: shown.length, total: state.rows.length })
    : lt('empty_structure');
}

function setInput({ view, unitTypes, myPermissions }) {
  state.view = view;
  state.unitTypes = unitTypes;
  state.myPermissions = myPermissions;
  state.units = new Map(view.units.map((u) => [u.unit_id, u]));
  state.rows = listRows(view, t);
}

function wireTable() {
  const table = state.host.querySelector('[data-role="table"]');
  if (!isAdmin()) return;
  table.setAttribute('actions-label', lt('col_actions'));
  // The builder reads the row at click time (`current()`), so a sort or a refresh under an open
  // menu cannot make it act on a neighbour.
  table.rowActionsKey = (row) => row._id;
  table.rowActions = (row, _index, current) => {
    const button = document.createElement('tf-button');
    button.setAttribute('variant', 'ghost');
    button.setAttribute('size', 'sm');
    button.setAttribute('icon', 'more');
    button.setAttribute('aria-label', lt('more'));
    button.setAttribute('title', lt('more'));
    button.dataset.rowId = row._id;
    button.addEventListener('click', () => openRowMenu(button, current()));
    return button;
  };
}

// ---------------------------------------------------------------------------
// Row menu and actions
// ---------------------------------------------------------------------------

// Rows are drawn again after every write and filter, so the button a window was opened from may be gone
// when it closes: focus goes back to the button of the same row by its key, wherever it is now.
function focusRow(id) {
  const table = state?.host.querySelector('[data-role="table"]');
  const button = [...(table?.shadowRoot?.querySelectorAll('tf-button[data-row-id]') ?? [])].find((b) => b.dataset.rowId === id);
  focusTarget(button ?? state?.host.querySelector('.org-list-search input'))?.focus();
}

function openRowMenu(button, entry) {
  const row = state.shown.get(entry?._id);
  if (!row) return;
  const anchor = { isConnected: true, focus: () => focusRow(row._id) };
  const run = {
    edit: (a) => state.actions.editAssignment(row, a),
    move: (a) => state.actions.moveAssignment(row, a),
    end: (a) => state.actions.endAssignment(row, a),
    handover: () => openHandover({ userId: row.subject.id, reason: 'departure' }),
    assign: (a) => state.actions.assignPerson(row, a),
    end_position: (a) => state.actions.endPosition(row, a),
    deputy: (a) => state.cover.addDeputy({
      userId: row.subject.id, userName: row.personName, today: state.view.at, anchor: a,
    }),
  };
  const items = menuItems(row, { isAdmin: isAdmin() }).map((item) => (item.separator
    ? item
    : { ...item, label: item.id === 'handover' ? t('handover.menu') : lt(`menu_${item.id}`), run: () => run[item.id](anchor) }));
  if (items.length) openActionMenu(button, items, row.vacant ? row.positionName : row.personName);
}

async function exportFile(format, anchor) {
  try {
    const body = await ApiBinary.one('orgExportRequest', { format, at: state.view.at });
    downloadBytes(body.file_name ?? body.fileName, body.bytes, body.mime);
  } catch (err) {
    TfToast.show({ tone: 'danger', message: lt('export_failed', { message: err.message || '' }) });
  }
  anchor?.focus?.();
}

function openExportMenu(anchor) {
  openActionMenu(anchor, [
    { label: lt('export_csv'), icon: 'file', run: () => exportFile('csv', anchor) },
    { label: lt('export_xlsx'), icon: 'file', run: () => exportFile('xlsx', anchor) },
  ]);
}

function openImport() {
  openImportPanel({
    day: state.view.at,
    unitTypes: state.unitTypes,
    onApplied: () => state.reload(),
  });
}

function onClick(e) {
  const target = e.target.closest('[data-act]');
  if (!target || target.hasAttribute('disabled')) return;
  switch (target.dataset.act) {
    case 'export': openExportMenu(target); break;
    case 'import': if (isAdmin()) openImport(); break;
    case 'add': if (isAdmin()) state.actions.addPosition(target); break;
    default: break;
  }
}

function wire(root) {
  root.addEventListener('click', onClick);
  root.addEventListener('search', (e) => {
    state.query = String(e.detail?.value ?? '');
    applyFilters();
  });
  root.addEventListener('change', (e) => {
    const role = e.target.closest?.('[data-role]')?.dataset.role;
    if (role === 'chips') {
      state.chip = String(e.detail?.id ?? 'all');
      applyFilters();
    } else if (role === 'since') {
      const day = String(e.detail?.value ?? '');
      if (!isIsoDay(day)) return;
      state.since = day;
      applyFilters();
    }
  });
}

// ---------------------------------------------------------------------------
// Public
// ---------------------------------------------------------------------------

/**
 * Draws the tab into `host`. `input` is a structure answer ({ view, unitTypes, myPermissions });
 * `reload()` re-reads the structure for the whole screen and hands the new answer back through
 * `refreshListTab`.
 */
export function mountListTab(host, input, { reload }) {
  unmountListTab();
  state = { host, chip: 'all', query: '', since: null, shown: new Map(), reload };
  setInput(input);
  state.since = addDays(state.view.at, -DEFAULT_CHANGES_DAYS);
  state.actions = createListActions({
    context: () => ({ view: state.view, rows: state.rows, day: state.view.at }),
    reload,
  });
  state.cover = createCoverActions({ reload, people: accountPeople() });
  const root = document.createElement('div');
  root.className = 'org-list';
  root.innerHTML = shell();
  host.replaceChildren(root);
  root.querySelector('[data-role="since"]').setAttribute('value', state.since);
  wire(root);
  wireTable();
  renderHeaderActions();
  applyFilters();
}

/** Feeds a newer structure answer to the tab on screen; the search, chip and sort stay as they are. */
export function refreshListTab(input) {
  if (!state) return;
  const wasAdmin = isAdmin();
  setInput(input);
  if (wasAdmin !== isAdmin()) {
    state.host.querySelector('.org-list').innerHTML = shell();
    wireTable();
    renderHeaderActions();
  }
  applyFilters();
}

export function unmountListTab() {
  const actions = byId('org-list-actions');
  if (actions) {
    actions.removeEventListener('click', onClick);
    actions.innerHTML = '';
  }
  state = null;
}
