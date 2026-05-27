// =============================================================================
// Plik: modules/scheduler.js
// Opis: Ekran admina do cyklicznego uruchamiania funkcji addonow przez scheduler.
// Przykład: Router.register('scheduler', SchedulerScreen)
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';

let jobs = [];
let actions = [];
let runs = [];
let selectedJobId = null;

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

const SchedulerScreen = {
  get title() { return 'Scheduler'; },

  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('clock')} Scheduler</h1>
          <div class="sub" id="scheduler-sub">Harmonogramy funkcji addonow</div>
        </div>
        <div class="actions">
          <tf-button variant="ghost" icon="refresh" id="scheduler-refresh">Odśwież</tf-button>
          <tf-button variant="primary" icon="plus" id="scheduler-new">Nowy job</tf-button>
        </div>
      </div>

      <div class="scheduler-layout">
        <section class="card" style="padding: 0; overflow: hidden;">
          <div class="scheduler-toolbar">
            <strong>Joby</strong>
            <span id="scheduler-count" class="muted">0</span>
          </div>
          <div id="scheduler-jobs" class="scheduler-list"></div>
        </section>

        <section class="card" style="padding: 0; overflow: hidden;">
          <div class="scheduler-toolbar">
            <strong>Konfiguracja</strong>
            <tf-button variant="primary" size="sm" icon="check" id="scheduler-save">Zapisz</tf-button>
          </div>
          <div id="scheduler-form" class="scheduler-form"></div>
          <div class="scheduler-toolbar">
            <strong>Ostatnie uruchomienia</strong>
          </div>
          <div id="scheduler-runs" class="scheduler-runs"></div>
        </section>
      </div>
    `;
  },

  async mount() {
    byId('scheduler-refresh')?.addEventListener('click', loadAll);
    byId('scheduler-new')?.addEventListener('click', () => {
      selectedJobId = null;
      runs = [];
      renderJobs();
      renderForm();
      renderRuns();
    });
    byId('scheduler-save')?.addEventListener('click', saveJob);
    renderForm();
    await loadAll();
  },

  unmount() {
    jobs = [];
    actions = [];
    runs = [];
    selectedJobId = null;
  },
};

async function loadAll() {
  try {
    const [jobRows, actionRows] = await Promise.all([
      ApiBinary.one('schedulerJobsListRequest').then(resp => parseJson(resp.jobsJson || resp.jobs_json || '[]')),
      ApiBinary.one('schedulerActionsListRequest').then(resp => parseJson(resp.actionsJson || resp.actions_json || '[]')),
    ]);
    jobs = Array.isArray(jobRows) ? jobRows : [];
    actions = Array.isArray(actionRows) ? actionRows : [];
    if (selectedJobId && !jobs.some(j => j.id === selectedJobId)) selectedJobId = null;
    if (!selectedJobId && jobs.length) selectedJobId = jobs[0].id;
    renderJobs();
    renderForm();
    await loadRuns();
    byId('scheduler-sub').textContent = `${actions.length} funkcji addonow dostepnych dla schedulera`;
  } catch (err) {
    toast(`Scheduler: ${err.message}`, 'error');
  }
}

async function loadRuns() {
  if (!selectedJobId) {
    runs = [];
    renderRuns();
    return;
  }
  try {
    const resp = await ApiBinary.one('schedulerRunsListRequest', { jobId: selectedJobId, limit: 20 });
    runs = parseJson(resp.runsJson || resp.runs_json || '[]');
    renderRuns();
  } catch (err) {
    runs = [];
    renderRuns();
    toast(`Historia uruchomien: ${err.message}`, 'error');
  }
}

function selectedJob() {
  return jobs.find(j => j.id === selectedJobId) || null;
}

function renderJobs() {
  const host = byId('scheduler-jobs');
  if (!host) return;
  byId('scheduler-count').textContent = `${jobs.length}`;
  if (!jobs.length) {
    host.innerHTML = `<div class="muted">Brak harmonogramow. Utworz pierwszy job po prawej stronie.</div>`;
    return;
  }
  host.innerHTML = jobs.map((job) => {
    const active = job.id === selectedJobId ? ' active' : '';
    const status = job.enabled ? 'ok' : 'warn';
    return `
      <article class="scheduler-job${active}" data-job-id="${escapeAttr(job.id)}">
        <div>
          <div class="scheduler-job-title">${escapeHtml(job.name)}</div>
          <div class="scheduler-job-meta">
            <span class="tf-chip ${status}">${escapeHtml(job.enabled ? 'enabled' : 'disabled')}</span>
            <span>${escapeHtml(job.target_addon_id)} / ${escapeHtml(job.target_action_id)}</span>
            <span>${escapeHtml(job.schedule_kind)}: ${escapeHtml(job.schedule_expr)}</span>
            <span>next: ${escapeHtml(formatIso(job.next_run_at))}</span>
          </div>
        </div>
        <div class="scheduler-job-actions">
          <tf-button variant="ghost" size="sm" icon="play" data-run="${escapeAttr(job.id)}" title="Uruchom teraz"></tf-button>
          <tf-button variant="danger" size="sm" icon="trash" data-delete="${escapeAttr(job.id)}" title="Usuń"></tf-button>
        </div>
      </article>
    `;
  }).join('');

  host.querySelectorAll('[data-job-id]').forEach((el) => {
    el.addEventListener('click', async (event) => {
      if (event.target.closest('[data-run], [data-delete]')) return;
      selectedJobId = el.dataset.jobId;
      renderJobs();
      renderForm();
      await loadRuns();
    });
  });
  host.querySelectorAll('[data-run]').forEach((el) => {
    el.addEventListener('click', async () => runJob(el.dataset.run));
  });
  host.querySelectorAll('[data-delete]').forEach((el) => {
    el.addEventListener('click', async () => deleteJob(el.dataset.delete));
  });
}

function renderForm() {
  const host = byId('scheduler-form');
  if (!host) return;
  const job = selectedJob();
  const addonId = job?.target_addon_id || firstAddonId();
  const actionId = job?.target_action_id || firstActionId(addonId);
  const actionValue = `${addonId}::${actionId}`;
  const kind = job?.schedule_kind || 'interval';
  const payload = job?.payload_json || '{}';
  const enabled = job?.enabled ?? true;
  const action = actionByValue(actionValue);
  const addonIds = uniqueAddonIds();

  host.innerHTML = `
    <div class="scheduler-selected-action" id="scheduler-action-summary">
      <span class="tf-chip info">${escapeHtml(action?.addon_id || 'addon')}</span>
      <strong>${escapeHtml(action?.display_name || action?.action_id || 'Wybierz funkcję')}</strong>
      <span>${escapeHtml(action?.description || '')}</span>
    </div>
    <div class="scheduler-form-grid">
      <tf-input id="scheduler-name" label="Nazwa" value="${escapeAttr(job?.name || '')}" placeholder="Dzienna synchronizacja"></tf-input>
      <div>
        <span class="tf-label">Addon</span>
        <tf-select id="scheduler-addon" value="${escapeAttr(addonId)}">
          ${addonIds.map(id => `
            <option value="${escapeAttr(id)}">
              ${escapeHtml(id)}
            </option>
          `).join('')}
        </tf-select>
      </div>

      <div>
        <span class="tf-label">Funkcja</span>
        <tf-select id="scheduler-action" value="${escapeAttr(actionId)}">
          ${actionsForAddon(addonId).map(item => `
            <option value="${escapeAttr(item.action_id)}">
              ${escapeHtml(item.display_name || item.action_id)}
            </option>
          `).join('')}
        </tf-select>
      </div>

      <div>
        <span class="tf-label">Status</span>
        <tf-select id="scheduler-enabled" value="${enabled ? 'true' : 'false'}">
          <option value="true">Włączony</option>
          <option value="false">Wyłączony</option>
        </tf-select>
      </div>
      <div>
        <span class="tf-label">Tryb</span>
        <tf-select id="scheduler-kind" value="${escapeAttr(kind)}">
          <option value="interval">Interval</option>
          <option value="cron">Cron dzienny</option>
          <option value="once">Jednorazowo</option>
        </tf-select>
      </div>

      <tf-input id="scheduler-expr" label="Wyrażenie" value="${escapeAttr(job?.schedule_expr || '1d')}" placeholder="1d albo 30m albo 0 3 * * *"></tf-input>
      <tf-input id="scheduler-timeout" type="number" label="Timeout sekund" value="${escapeAttr(job?.max_runtime_seconds || 1800)}" min="1" max="86400"></tf-input>

      <tf-input class="scheduler-form-full" id="scheduler-payload" multiline rows="10" label="Payload JSON" value="${escapeAttr(payload)}"></tf-input>
    </div>
  `;

  byId('scheduler-addon')?.addEventListener('change', () => {
    const selectedAddon = byId('scheduler-addon')?.value || '';
    const actionSelect = byId('scheduler-action');
    if (actionSelect) {
      actionSelect.innerHTML = actionsForAddon(selectedAddon).map(item => `
        <option value="${escapeAttr(item.action_id)}">
          ${escapeHtml(item.display_name || item.action_id)}
        </option>
      `).join('');
      actionSelect.value = firstActionId(selectedAddon);
    }
    resetPayloadFromSchema();
    updateActionSummary();
  });
  byId('scheduler-action')?.addEventListener('change', () => {
    updateActionSummary();
    resetPayloadFromSchema();
  });
  if (!job && actions.length) resetPayloadFromSchema();
}

function updateActionSummary() {
  const host = byId('scheduler-action-summary');
  if (!host) return;
  const action = selectedAction();
  host.innerHTML = `
    <span class="tf-chip info">${escapeHtml(action?.addon_id || 'addon')}</span>
    <strong>${escapeHtml(action?.display_name || action?.action_id || 'Wybierz funkcję')}</strong>
    <span>${escapeHtml(action?.description || '')}</span>
  `;
}

function resetPayloadFromSchema() {
  const payloadEl = byId('scheduler-payload');
  if (!payloadEl) return;
  const action = selectedAction();
  const schema = parseJson(action?.parameters_schema);
  const props = schema?.properties || {};
  const sample = {};
  for (const [key, def] of Object.entries(props)) {
    sample[key] = sampleForType(def?.type);
  }
  payloadEl.value = JSON.stringify(sample, null, 2);
}

async function saveJob() {
  const addonId = byId('scheduler-addon')?.value || '';
  const actionId = byId('scheduler-action')?.value || '';
  const payloadEl = byId('scheduler-payload');
  const payloadValue = payloadEl?.value || '{}';
  try {
    const parsedPayload = JSON.parse(payloadValue);
    const body = {
      id: selectedJobId,
      name: byId('scheduler-name')?.value?.trim() || `${addonId} / ${actionId}`,
      enabled: byId('scheduler-enabled')?.value === 'true',
      target_addon_id: addonId || '',
      target_action_id: actionId || '',
      payload_json: JSON.stringify(parsedPayload),
      schedule_kind: byId('scheduler-kind')?.value || 'interval',
      schedule_expr: byId('scheduler-expr')?.value?.trim() || '1d',
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
      max_runtime_seconds: Number(byId('scheduler-timeout')?.value || 1800),
      retry_policy_json: JSON.stringify({ max_attempts: 1, backoff_seconds: 60 }),
      concurrency_policy: 'skip',
    };
    const resp = await ApiBinary.one('schedulerJobUpsertRequest', { jobJson: JSON.stringify(body) });
    const saved = parseJson(resp.jobJson || resp.job_json || '{}');
    selectedJobId = saved.id;
    toast('Job zapisany', 'success');
    await loadAll();
  } catch (err) {
    toast(`Zapis schedulera: ${err.message}`, 'error');
  }
}

async function runJob(jobId) {
  try {
    await ApiBinary.one('schedulerJobRunNowRequest', { jobId });
    toast('Job uruchomiony', 'success');
    if (selectedJobId === jobId) await loadRuns();
    await loadAll();
  } catch (err) {
    toast(`Uruchomienie joba: ${err.message}`, 'error');
  }
}

async function deleteJob(jobId) {
  try {
    await ApiBinary.one('schedulerJobDeleteRequest', { jobId });
    if (selectedJobId === jobId) selectedJobId = null;
    toast('Job usunięty', 'success');
    await loadAll();
  } catch (err) {
    toast(`Usuwanie joba: ${err.message}`, 'error');
  }
}

function renderRuns() {
  const host = byId('scheduler-runs');
  if (!host) return;
  if (!selectedJobId) {
    host.innerHTML = `<div class="muted">Wybierz job, żeby zobaczyć historię.</div>`;
    return;
  }
  if (!runs.length) {
    host.innerHTML = `<div class="muted">Brak uruchomień.</div>`;
    return;
  }
  host.innerHTML = runs.map((run) => `
    <article class="scheduler-run">
      <div>
        <span class="tf-chip ${statusClass(run.status)}">${escapeHtml(run.status)}</span>
        <span>${escapeHtml(formatIso(run.scheduled_for))}</span>
      </div>
      <div class="scheduler-run-meta">
        <span>start: ${escapeHtml(formatIso(run.started_at))}</span>
        <span>koniec: ${escapeHtml(formatIso(run.finished_at))}</span>
        ${run.error ? `<span>${escapeHtml(run.error)}</span>` : ''}
      </div>
    </article>
  `).join('');
}

function firstAddonId() {
  return uniqueAddonIds()[0] || '';
}

function firstActionId(addonId) {
  return actionsForAddon(addonId)[0]?.action_id || '';
}

function uniqueAddonIds() {
  return [...new Set(actions.map(action => action.addon_id).filter(Boolean))].sort();
}

function actionsForAddon(addonId) {
  return actions
    .filter(action => action.addon_id === addonId)
    .sort((a, b) => String(a.display_name || a.action_id).localeCompare(String(b.display_name || b.action_id)));
}

function selectedAction() {
  const addonId = byId('scheduler-addon')?.value || '';
  const actionId = byId('scheduler-action')?.value || '';
  return actions.find(action => action.addon_id === addonId && action.action_id === actionId) || null;
}

function actionByValue(value) {
  const [addonId, actionId] = value.split('::');
  return actions.find(a => a.addon_id === addonId && a.action_id === actionId) || null;
}

function parseJson(value) {
  try { return JSON.parse(value || '{}'); } catch (_) { return {}; }
}

function sampleForType(type) {
  if (type === 'number' || type === 'integer') return 0;
  if (type === 'boolean') return false;
  if (type === 'array') return [];
  if (type === 'object') return {};
  return '';
}

function statusClass(status) {
  if (status === 'success') return 'ok';
  if (status === 'failed' || status === 'timeout') return 'err';
  if (status === 'running') return 'info';
  return 'warn';
}

function formatIso(value) {
  if (!value) return '—';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(I18n.getLanguage());
}

export default SchedulerScreen;
