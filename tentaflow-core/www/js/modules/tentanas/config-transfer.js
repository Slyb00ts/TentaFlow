// ===== File: modules/tentanas/config-transfer.js — the TentaNas configuration window (N18b): export preview + download, import plan → apply job; the picker is shared with the first-run wizard =====
//
// §5.8: the export is the desired state without secrets; the import first
// asks the node for a plan (what would be imported, created, updated,
// skipped or conflicts) and only applies after the admin confirms it.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, ADMIN_TIMEOUT_MS, errMessage, jobKindLabel } from '/js/modules/tentanas/format.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-file-input.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-segmented.js';

const ACTION_TONE = { import: 'ok', create: 'ok', update: 'info', skip: 'neutral', conflict: 'err', missing: 'warn' };
const KIND_ICON = { pool: 'database', dataset: 'folder', share: 'share', share_user: 'user', schedule: 'clock' };

/** Hands a text blob to the browser as a file download; the object URL is released once the click has been dispatched. */
export function downloadText(text, filename, type = 'application/json') {
  const blob = new Blob([text], { type });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.hidden = true;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/** Opens the configuration window on its export side; the download starts from the window's "Pobierz plik". */
export function exportConfig(screen, { onDone = null } = {}) {
  return openConfigWindow(screen, { segment: 'export', onDone });
}

/** Opens the configuration window on its import side: pick a file, review the plan, apply. */
export function openConfigImportDialog(screen, { onDone = null } = {}) {
  return openConfigWindow(screen, { segment: 'import', onDone });
}

const PREVIEW_LINES = 40;

// The preview shows the head of the export with keys and strings tinted the
// way the mockup does; the text is escaped before the spans go in.
export function jsonPreviewHtml(json, maxLines = PREVIEW_LINES) {
  const lines = String(json || '').split('\n');
  const head = lines.slice(0, maxLines).join('\n');
  let out = '';
  let last = 0;
  const re = /"(?:[^"\\]|\\.)*"(\s*:)?/g;
  let m;
  while ((m = re.exec(head))) {
    out += escapeHtml(head.slice(last, m.index));
    const str = m[1] ? m[0].slice(0, -m[1].length) : m[0];
    out += `<span class="${m[1] ? 'k' : 's'}">${escapeHtml(str)}</span>${m[1] ? escapeHtml(m[1]) : ''}`;
    last = m.index + m[0].length;
  }
  out += escapeHtml(head.slice(last));
  return out + (lines.length > maxLines ? '\n…' : '');
}

/**
 * The N18b window: one tf-window with an Eksport | Import segmented control.
 * The export side fetches the desired state as soon as it shows and keeps
 * it for the download; the import side is the picker + plan of
 * `mountImportPicker`.
 */
export function openConfigWindow(screen, { segment = 'export', onDone = null } = {}) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('config.window_title'));
  win.setAttribute('icon', 'download');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '640');
  win.setAttribute('min-width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const state = { segment, exportRes: null, exportError: '', json: null, plan: null, busy: false };
  win.innerHTML = `
    <div slot="body" class="stack">
      <tf-segmented id="nas-cfg-segment" value="${escapeAttr(segment)}" size="md">
        <option value="export">${escapeHtml(T('config.segment_export'))}</option>
        <option value="import">${escapeHtml(T('config.segment_import'))}</option>
      </tf-segmented>
      <div id="nas-cfg-export" class="stack">
        <div class="explain-box">${T('config.preview_explain')}</div>
        <details open>
          <summary class="text-sm text-2 fw-700">${escapeHtml(T('config.preview'))}</summary>
          <pre class="json-pre" id="nas-cfg-preview">${escapeHtml(I18n.t('common.loading'))}</pre>
        </details>
      </div>
      <div id="nas-cfg-import" class="stack" hidden>
        <div class="explain-box">${escapeHtml(T('config.import_explain'))}</div>
        <div id="nas-ci-picker"></div>
      </div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <span class="spacer" style="flex:1"></span>
      <tf-button variant="primary" icon="download" data-act="download" disabled>${escapeHtml(T('config.download'))}</tf-button>
      <tf-button variant="primary" icon="check" data-act="apply" disabled hidden>${escapeHtml(T('config.apply'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const downloadBtn = win.querySelector('[data-act="download"]');
  const applyBtn = win.querySelector('[data-act="apply"]');

  const showSegment = () => {
    const exp = state.segment === 'export';
    win.querySelector('#nas-cfg-export').hidden = !exp;
    win.querySelector('#nas-cfg-import').hidden = exp;
    downloadBtn.hidden = !exp;
    applyBtn.hidden = exp;
  };
  const syncButtons = () => {
    if (state.exportRes && !state.busy) downloadBtn.removeAttribute('disabled'); else downloadBtn.setAttribute('disabled', '');
    const ready = Boolean(state.json && state.plan && state.plan.items.length && !planBlocked(state.plan.items));
    if (ready && !state.busy) applyBtn.removeAttribute('disabled'); else applyBtn.setAttribute('disabled', '');
  };
  win.querySelector('#nas-cfg-segment').addEventListener('change', (e) => { state.segment = e.detail.value; showSegment(); });
  showSegment();

  const preview = win.querySelector('#nas-cfg-preview');
  screen.nas('tentaNasConfigExportRequest', {}).then((res) => {
    state.exportRes = res;
    if (win.isConnected) preview.innerHTML = jsonPreviewHtml(res.json);
    syncButtons();
  }).catch((e) => {
    state.exportError = errMessage(e);
    if (win.isConnected) { preview.className = 'num-err'; preview.textContent = T('config.export_failed', { error: state.exportError }); }
  });

  mountImportPicker(screen, win.querySelector('#nas-ci-picker'), {
    onState: (st) => { state.json = st.json; state.plan = st.plan; syncButtons(); },
  });

  downloadBtn.addEventListener('click', () => {
    if (!state.exportRes) return;
    const filename = state.exportRes.filename || 'tentanas-config.json';
    downloadText(state.exportRes.json, filename);
    toast(T('config.exported', { file: filename }), 'success');
    win.close(true);
  });
  applyBtn.addEventListener('click', async () => {
    if (!state.json || planBlocked(state.plan?.items) || state.busy) return;
    state.busy = true;
    syncButtons();
    const started = await applyImport(screen, state.json, onDone);
    state.busy = false;
    if (started) win.close(true); else syncButtons();
  });
  win.addEventListener('action', (e) => { if (e.detail?.action === 'cancel') win.close(true); });
  return win;
}

/** Reads a picked file as text and checks it parses as JSON; throws with an i18n message otherwise. */
export async function readJsonFile(file) {
  const text = await file.text();
  try { JSON.parse(text); } catch { throw new Error(T('config.not_json', { file: file.name })); }
  return text;
}

export const planCounts = (items) => (items || []).reduce((acc, it) => { acc[it.action] = (acc[it.action] || 0) + 1; return acc; }, {});
export const planBlocked = (items) => (items || []).some((it) => it.action === 'conflict');

/** Paints the plan (summary chips, table, warnings) into `host`; `items` and `warnings` come straight from ConfigImportPlanResponse. */
export function renderImportPlan(host, { items = [], warnings = [] }) {
  const counts = planCounts(items);
  const summary = ['import', 'create', 'update', 'skip', 'conflict', 'missing']
    .filter((a) => counts[a])
    .map((a) => `<tf-chip size="sm" status="${ACTION_TONE[a]}" dot label="${escapeAttr(T('config.action_' + a))}: ${counts[a]}"></tf-chip>`)
    .join(' ');
  host.innerHTML = `
    <div class="row" id="nas-ci-summary">${summary || `<span class="muted">${escapeHtml(T('config.plan_empty'))}</span>`}</div>
    ${items.length ? `
    <tf-table id="nas-ci-plan">
      <tf-column key="kind" label="${escapeAttr(T('config.col_kind'))}" renderer="html" nowrap></tf-column>
      <tf-column key="name" label="${escapeAttr(T('config.col_name'))}" renderer="html"></tf-column>
      <tf-column key="action" label="${escapeAttr(T('config.col_action'))}" renderer="chip" nowrap></tf-column>
      <tf-column key="detail" label="${escapeAttr(T('config.col_detail'))}" renderer="text" fill></tf-column>
    </tf-table>` : ''}
    ${warnings.map((w) => `<div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(w)}</div></div>`).join('')}
    ${planBlocked(items) ? `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('config.plan_conflicts'))}</div></div>` : ''}`;
  const table = host.querySelector('#nas-ci-plan');
  if (table) {
    table.rows = items.map((it) => ({
      kind: `<span class="tf-table__cell-row">${sprite(KIND_ICON[it.kind] || 'file')}${escapeHtml(T('config.kind_' + it.kind))}</span>`,
      name: `<span class="tf-table__cell--mono">${escapeHtml(it.name)}</span>`,
      action: { status: ACTION_TONE[it.action] || 'info', label: T('config.action_' + it.action), dot: true },
      detail: it.detail || '',
    }));
  }
}

/**
 * The file picker + plan section used both by the import dialog and by the
 * first-run wizard's "Odtwórz z kopii" step. `host` is the element that holds
 * the markup; `onState` gets `{ json, plan }` whenever the plan changes so the
 * caller can enable its Apply button.
 */
export function mountImportPicker(screen, host, { onState }) {
  const state = { json: null, plan: null, file: '' };
  host.innerHTML = `
    <div class="stack">
      <tf-file-input id="nas-ci-file" accept="application/json,.json" label="${escapeAttr(T('config.pick_file'))}"></tf-file-input>
      <div id="nas-ci-status" class="muted">${escapeHtml(T('config.pick_hint'))}</div>
      <div id="nas-ci-plan-box"></div>
    </div>`;
  const statusEl = host.querySelector('#nas-ci-status');
  const planBox = host.querySelector('#nas-ci-plan-box');
  const emit = () => onState({ json: state.json, plan: state.plan, file: state.file });
  host.querySelector('#nas-ci-file').addEventListener('change', async (e) => {
    const file = e.detail?.files?.[0];
    if (!file) return;
    state.json = null; state.plan = null; state.file = file.name;
    planBox.innerHTML = '';
    statusEl.className = 'muted';
    statusEl.textContent = T('config.planning', { file: file.name });
    emit();
    try {
      const json = await readJsonFile(file);
      const plan = await screen.nas('tentaNasConfigImportPlanRequest', { json });
      state.json = json;
      state.plan = { items: plan.items || [], warnings: plan.warnings || [] };
      statusEl.textContent = T('config.plan_ready', { file: file.name, n: state.plan.items.length });
      renderImportPlan(planBox, state.plan);
    } catch (err) {
      statusEl.className = 'num-err';
      statusEl.textContent = errMessage(err);
    }
    emit();
  });
  return state;
}

/** Sends the confirmed JSON as an import job through sudo; opens the job log. Returns true when the job started. */
export async function applyImport(screen, json, onDone) {
  let res;
  try {
    res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasConfigImportApplyRequest', { json, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('config.sudo_apply'));
  } catch (e) {
    toast(errMessage(e), 'error');
    return false;
  }
  if (!res || !res.job) return false;
  toast(T('jobs.started', { kind: jobKindLabel(res.job.kind) }), 'success');
  screen.openJobLog(res.job.jobId, onDone);
  return true;
}
