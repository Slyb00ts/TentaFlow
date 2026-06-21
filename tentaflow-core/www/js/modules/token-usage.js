// =============================================================================
// Plik: modules/token-usage.js
// Opis: Ekran admina metryk tokenów — zużycie (wykresy + tabela), limity (quota)
//       z edytorem oraz status koordynatora dzierżaw (lease). Binary CBOR / WS.
// Przykład: Router.register('token-usage', TokenUsageScreen)
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-combobox.js';
import '/js/components/tf-datepicker.js';
import '/js/components/tf-modal.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-bar-chart.js';
import '/js/components/tf-line-chart.js';

let me = null;
let activeTab = 'usage';

// Filtry zakładki "Zużycie".
let usageFilters = { period: 'daily', groupBy: 'user', periodKey: todayIso() };
let usageRows = [];

// Dane zakładki "Limity".
let quotas = [];
// Listy podmiotów do comboboxów (ładowane raz przy montażu).
let usersList = [];
let groupsList = [];
let modelsList = [];

// Dane zakładki "Koordynator".
let coordinator = { nodeId: null, leases: [] };

const T = (key, params) => I18n.t(`token_usage.${key}`, params);

function todayIso() {
  const d = new Date();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${d.getFullYear()}-${m}-${day}`;
}

function currentMonth() {
  const d = new Date();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  return `${d.getFullYear()}-${m}`;
}

// Lista ostatnich 12 miesięcy dla selektora miesięcznego.
function recentMonths() {
  const out = [];
  const d = new Date();
  for (let i = 0; i < 12; i += 1) {
    const m = String(d.getMonth() + 1).padStart(2, '0');
    out.push(`${d.getFullYear()}-${m}`);
    d.setMonth(d.getMonth() - 1);
  }
  return out;
}

const TokenUsageScreen = {
  get title() { return T('title'); },

  render() {
    return `<div id="token-usage-root"></div>`;
  },

  async mount() {
    try {
      me = await ApiBinary.one('authMeRequest');
    } catch {
      me = null;
    }
    const root = byId('token-usage-root');
    if (!root) return;
    if (!me || (me.role !== 'admin' && !me.isAdmin)) {
      root.innerHTML = `<div class="card"><p>${escapeHtml(T('admin_only'))}</p></div>`;
      return;
    }
    root.innerHTML = shellHtml();
    byId('tu-tabs')?.addEventListener('change', (e) => {
      const id = e.detail?.value;
      if (id) switchTab(id);
    });
    await loadSubjects();
    await switchTab('usage');
  },

  unmount() {
    me = null;
    usageRows = [];
    quotas = [];
    usersList = [];
    groupsList = [];
    modelsList = [];
    coordinator = { nodeId: null, leases: [] };
  },
};

function shellHtml() {
  return `
    <div class="page-header">
      <div>
        <h1>${escapeHtml(T('title'))}</h1>
        <div class="sub">${escapeHtml(T('subtitle'))}</div>
      </div>
    </div>
    <tf-tabs variant="solid" value="usage" id="tu-tabs">
      <tf-tab id="usage">${escapeHtml(T('tab_usage'))}</tf-tab>
      <tf-tab id="quotas">${escapeHtml(T('tab_quotas'))}</tf-tab>
      <tf-tab id="coordinator">${escapeHtml(T('tab_coordinator'))}</tf-tab>
    </tf-tabs>
    <div id="tu-panel"></div>
  `;
}

async function switchTab(tab) {
  activeTab = tab;
  if (tab === 'usage') {
    renderUsagePanel();
    await loadUsage();
  } else if (tab === 'quotas') {
    renderQuotasPanel();
    await loadQuotas();
  } else if (tab === 'coordinator') {
    renderCoordinatorPanel();
    await loadCoordinator();
  }
}

// ---------------------------------------------------------------------------
// Listy podmiotów (users / groups / models) dla comboboxów limitów.
// ---------------------------------------------------------------------------

async function loadSubjects() {
  const [u, g, m] = await Promise.all([
    ApiBinary.action('iamListUsersRequest').then((r) => r?.users ?? []).catch(() => []),
    ApiBinary.action('iamListGroupsRequest').then((r) => r?.groups ?? []).catch(() => []),
    ApiBinary.list('modelListRequest', { arrayKey: 'models' }).catch(() => []),
  ]);
  usersList = Array.isArray(u) ? u : [];
  groupsList = Array.isArray(g) ? g : [];
  modelsList = (Array.isArray(m) ? m : [])
    .map((row) => {
      const value = row.model_name || row.modelName || '';
      const display = row.display_name || row.displayName || value;
      return { value, label: display };
    })
    .filter((o) => o.value);
}

function userOptions() {
  return usersList.map((u) => ({
    value: u.id,
    label: u.displayName || u.username || u.email || u.id,
    description: u.email || '',
  }));
}

function groupOptions() {
  return groupsList.map((g) => ({ value: g.id, label: g.name || g.id }));
}

function modelOptions() {
  return modelsList.map((m) => ({ value: m.value, label: m.label }));
}

function subjectLabel(scopeType, subjectId) {
  if (!subjectId) return '—';
  if (scopeType === 'user') {
    const u = usersList.find((x) => x.id === subjectId);
    return u ? (u.displayName || u.username || u.email || subjectId) : subjectId;
  }
  if (scopeType === 'group') {
    const g = groupsList.find((x) => x.id === subjectId);
    return g ? (g.name || subjectId) : subjectId;
  }
  if (scopeType === 'model') {
    const m = modelsList.find((x) => x.value === subjectId);
    return m ? m.label : subjectId;
  }
  return subjectId;
}

// ---------------------------------------------------------------------------
// Zakładka "Zużycie".
// ---------------------------------------------------------------------------

function renderUsagePanel() {
  const panel = byId('tu-panel');
  if (!panel) return;
  const isMonthly = usageFilters.period === 'monthly';
  panel.innerHTML = `
    <section class="card">
      <div class="tu-filters">
        <tf-select id="tu-period" label="${escapeAttr(T('period'))}" value="${escapeAttr(usageFilters.period)}">
          <option value="daily">${escapeHtml(T('period_daily'))}</option>
          <option value="monthly">${escapeHtml(T('period_monthly'))}</option>
        </tf-select>
        <tf-select id="tu-group" label="${escapeAttr(T('group_by'))}" value="${escapeAttr(usageFilters.groupBy)}">
          <option value="user">${escapeHtml(T('group_user'))}</option>
          <option value="model">${escapeHtml(T('group_model'))}</option>
          <option value="day">${escapeHtml(T('group_day'))}</option>
        </tf-select>
        <div id="tu-period-key-host">${periodKeyControlHtml(isMonthly)}</div>
        <tf-button variant="primary" icon="refresh" id="tu-refresh">${escapeHtml(T('refresh'))}</tf-button>
      </div>
      <div id="tu-chart-host" class="tu-chart-host"></div>
    </section>
    <section class="card" style="padding: 0; overflow: hidden;">
      <tf-table id="tu-usage-table">
        <tf-column key="key" label="${escapeAttr(T('col_key'))}" sortable></tf-column>
        <tf-column key="prompt" label="${escapeAttr(T('col_prompt'))}" renderer="num" sortable></tf-column>
        <tf-column key="completion" label="${escapeAttr(T('col_completion'))}" renderer="num" sortable></tf-column>
        <tf-column key="total" label="${escapeAttr(T('col_total'))}" renderer="num" sortable></tf-column>
        <tf-column key="requests" label="${escapeAttr(T('col_requests'))}" renderer="num" sortable></tf-column>
      </tf-table>
    </section>
  `;

  byId('tu-period')?.addEventListener('change', (e) => {
    usageFilters.period = e.detail?.value || 'daily';
    usageFilters.periodKey = usageFilters.period === 'monthly' ? currentMonth() : todayIso();
    const host = byId('tu-period-key-host');
    if (host) {
      host.innerHTML = periodKeyControlHtml(usageFilters.period === 'monthly');
      wirePeriodKey();
    }
  });
  byId('tu-group')?.addEventListener('change', (e) => {
    usageFilters.groupBy = e.detail?.value || 'user';
  });
  byId('tu-refresh')?.addEventListener('click', loadUsage);
  wirePeriodKey();
}

function periodKeyControlHtml(isMonthly) {
  if (isMonthly) {
    const months = recentMonths();
    return `
      <tf-select id="tu-period-key" label="${escapeAttr(T('period_key_month'))}" value="${escapeAttr(usageFilters.periodKey)}">
        ${months.map((mo) => `<option value="${escapeAttr(mo)}">${escapeHtml(mo)}</option>`).join('')}
      </tf-select>
    `;
  }
  return `<tf-datepicker id="tu-period-key" label="${escapeAttr(T('period_key'))}" value="${escapeAttr(usageFilters.periodKey)}"></tf-datepicker>`;
}

function wirePeriodKey() {
  byId('tu-period-key')?.addEventListener('change', (e) => {
    usageFilters.periodKey = e.detail?.value || usageFilters.periodKey;
  });
}

async function loadUsage() {
  try {
    const resp = await ApiBinary.one('tokenUsageSummaryRequest', {
      period: usageFilters.period,
      periodKey: usageFilters.periodKey,
      groupBy: usageFilters.groupBy,
    });
    usageRows = Array.isArray(resp.rows) ? resp.rows : [];
    renderUsageTable();
    renderUsageChart();
  } catch (err) {
    usageRows = [];
    renderUsageTable();
    renderUsageChart();
    toast(err.message || T('load_failed'), 'error');
  }
}

function renderUsageTable() {
  const table = byId('tu-usage-table');
  if (!table) return;
  table.rows = usageRows.map((r) => ({
    key: r.key,
    prompt: Number(r.prompt_tokens ?? r.promptTokens ?? 0),
    completion: Number(r.completion_tokens ?? r.completionTokens ?? 0),
    total: Number(r.total_tokens ?? r.totalTokens ?? 0),
    requests: Number(r.request_count ?? r.requestCount ?? 0),
  }));
}

function renderUsageChart() {
  const host = byId('tu-chart-host');
  if (!host) return;
  if (!usageRows.length) {
    host.innerHTML = `<div class="muted">${escapeHtml(T('no_usage'))}</div>`;
    return;
  }
  host.innerHTML = '';
  const points = usageRows.map((r) => ({
    x: r.key,
    y: Number(r.total_tokens ?? r.totalTokens ?? 0),
  }));
  const series = [{
    id: 'total',
    name: T('chart_total_tokens'),
    tone: 'primary',
    showInLegend: true,
    points,
  }];

  const chart = document.createElement(
    usageFilters.groupBy === 'day' ? 'tf-line-chart' : 'tf-bar-chart',
  );
  chart.xAxis = { scale: 'category' };
  chart.yAxis = { scale: 'linear' };
  chart.height = 260;
  chart.locale = I18n.getLanguage();
  chart.series = series;
  host.appendChild(chart);
}

// ---------------------------------------------------------------------------
// Zakładka "Limity".
// ---------------------------------------------------------------------------

function renderQuotasPanel() {
  const panel = byId('tu-panel');
  if (!panel) return;
  panel.innerHTML = `
    <div class="page-header">
      <div></div>
      <div class="actions">
        <tf-button variant="primary" icon="plus" id="tu-add-quota">${escapeHtml(T('add_quota'))}</tf-button>
      </div>
    </div>
    <section class="card" style="padding: 0; overflow: hidden;">
      <tf-table id="tu-quotas-table">
        <tf-column key="scope" label="${escapeAttr(T('col_scope'))}" renderer="chip"></tf-column>
        <tf-column key="subject" label="${escapeAttr(T('col_subject'))}"></tf-column>
        <tf-column key="model" label="${escapeAttr(T('col_model'))}"></tf-column>
        <tf-column key="period" label="${escapeAttr(T('col_period'))}"></tf-column>
        <tf-column key="limit" label="${escapeAttr(T('col_limit'))}" renderer="num"></tf-column>
        <tf-column key="active" label="${escapeAttr(T('col_active'))}" renderer="chip"></tf-column>
      </tf-table>
    </section>
  `;
  byId('tu-add-quota')?.addEventListener('click', () => openQuotaEditor(null));
  const table = byId('tu-quotas-table');
  if (table) table.rowActions = (row) => buildQuotaActions(row);
}

function buildQuotaActions(row) {
  const quota = row?._quota;
  if (!quota) return null;
  const wrap = document.createElement('div');
  wrap.style.display = 'flex';
  wrap.style.gap = '0.25rem';
  const edit = document.createElement('tf-button');
  edit.setAttribute('variant', 'ghost');
  edit.setAttribute('size', 'sm');
  edit.setAttribute('icon', 'edit');
  edit.title = T('edit');
  edit.addEventListener('click', () => openQuotaEditor(quota));
  const del = document.createElement('tf-button');
  del.setAttribute('variant', 'danger');
  del.setAttribute('size', 'sm');
  del.setAttribute('icon', 'trash');
  del.title = T('delete');
  del.addEventListener('click', () => confirmDeleteQuota(quota));
  wrap.append(edit, del);
  return wrap;
}

async function loadQuotas() {
  try {
    const resp = await ApiBinary.one('tokenListQuotasRequest');
    quotas = Array.isArray(resp.quotas) ? resp.quotas : [];
    renderQuotasTable();
  } catch (err) {
    quotas = [];
    renderQuotasTable();
    toast(err.message || T('load_failed'), 'error');
  }
}

function renderQuotasTable() {
  const table = byId('tu-quotas-table');
  if (!table) return;
  table.rows = quotas.map((q) => {
    const scopeType = q.scope_type ?? q.scopeType ?? '';
    const subjectId = q.subject_id ?? q.subjectId ?? null;
    const modelId = q.model_id ?? q.modelId ?? null;
    return {
      scope: T(`scope_${scopeType}`),
      subject: scopeType === 'org' ? '—' : subjectLabel(scopeType, subjectId),
      model: modelId ? (modelsList.find((m) => m.value === modelId)?.label || modelId) : T('all_models'),
      period: q.period === 'monthly' ? T('period_monthly') : T('period_daily'),
      limit: Number(q.max_total_tokens ?? q.maxTotalTokens ?? 0),
      active: (q.is_active ?? q.isActive) ? T('active_yes') : T('active_no'),
      _quota: q,
    };
  });
}

function openQuotaEditor(quota) {
  const isEdit = !!quota;
  const scopeType = quota?.scope_type ?? quota?.scopeType ?? 'user';
  const subjectId = quota?.subject_id ?? quota?.subjectId ?? '';
  const modelId = quota?.model_id ?? quota?.modelId ?? '';
  const period = quota?.period ?? 'daily';
  const maxTokens = Number(quota?.max_total_tokens ?? quota?.maxTotalTokens ?? 0);
  const isActive = quota ? !!(quota.is_active ?? quota.isActive) : true;
  const quotaId = quota?.id ?? null;

  const body = document.createElement('div');
  body.className = 'tu-quota-form';
  body.innerHTML = `
    <tf-select id="tu-q-scope" label="${escapeAttr(T('field_scope'))}" value="${escapeAttr(scopeType)}">
      <option value="user">${escapeHtml(T('scope_user'))}</option>
      <option value="group">${escapeHtml(T('scope_group'))}</option>
      <option value="model">${escapeHtml(T('scope_model'))}</option>
      <option value="org">${escapeHtml(T('scope_org'))}</option>
    </tf-select>
    <div id="tu-q-subject-host"></div>
    <tf-combobox id="tu-q-model" label="${escapeAttr(T('field_model'))}" clearable
      placeholder="${escapeAttr(T('field_model_any'))}"></tf-combobox>
    <tf-select id="tu-q-period" label="${escapeAttr(T('field_period'))}" value="${escapeAttr(period)}">
      <option value="daily">${escapeHtml(T('period_daily'))}</option>
      <option value="monthly">${escapeHtml(T('period_monthly'))}</option>
    </tf-select>
    <tf-input id="tu-q-max" type="number" min="1" label="${escapeAttr(T('field_max_tokens'))}"
      value="${escapeAttr(String(maxTokens || ''))}"></tf-input>
    <tf-toggle id="tu-q-active" label="${escapeAttr(T('field_active'))}" ${isActive ? 'checked' : ''}></tf-toggle>
  `;

  const editor = {
    scopeType, subjectId, modelId, quotaId,
  };

  const modal = document.createElement('tf-modal');
  modal.setAttribute('title', T(isEdit ? 'quota_edit_title' : 'quota_new_title'));
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('size', 'md');
  const bodySlot = document.createElement('div');
  bodySlot.setAttribute('slot', 'body');
  bodySlot.appendChild(body);
  modal.appendChild(bodySlot);

  const footerSlot = document.createElement('div');
  footerSlot.setAttribute('slot', 'footer');
  const cancelBtn = document.createElement('tf-button');
  cancelBtn.setAttribute('variant', 'secondary');
  cancelBtn.textContent = T('cancel');
  cancelBtn.addEventListener('click', () => closeModal(modal));
  const saveBtn = document.createElement('tf-button');
  saveBtn.setAttribute('variant', 'primary');
  saveBtn.textContent = T('save');
  saveBtn.addEventListener('click', () => saveQuota(modal, editor));
  footerSlot.append(cancelBtn, saveBtn);
  modal.appendChild(footerSlot);

  document.body.appendChild(modal);
  modal.setAttribute('open', '');

  // Combobox modeli wypełniamy po dołączeniu do DOM (property, nie atrybut).
  const modelBox = body.querySelector('#tu-q-model');
  if (modelBox) {
    modelBox.options = modelOptions();
    if (modelId) modelBox.value = modelId;
    modelBox.addEventListener('change', (e) => {
      editor.modelId = e.detail?.value || '';
    });
  }

  const scopeSel = body.querySelector('#tu-q-scope');
  scopeSel?.addEventListener('change', (e) => {
    editor.scopeType = e.detail?.value || 'user';
    editor.subjectId = '';
    renderSubjectControl(body, editor);
  });
  renderSubjectControl(body, editor);

  modal.addEventListener('close', () => closeModal(modal), { once: true });
}

function renderSubjectControl(body, editor) {
  const host = body.querySelector('#tu-q-subject-host');
  if (!host) return;
  if (editor.scopeType === 'org') {
    host.innerHTML = '';
    return;
  }
  let options = [];
  if (editor.scopeType === 'user') options = userOptions();
  else if (editor.scopeType === 'group') options = groupOptions();
  else if (editor.scopeType === 'model') options = modelOptions();

  // Gdy lista jest pusta (np. brak modeli), pozwalamy wpisać id ręcznie.
  if (!options.length) {
    host.innerHTML = `<tf-input id="tu-q-subject" label="${escapeAttr(T('field_subject'))}"
      placeholder="${escapeAttr(T('field_subject_hint'))}" value="${escapeAttr(editor.subjectId || '')}"></tf-input>`;
    host.querySelector('#tu-q-subject')?.addEventListener('change', (e) => {
      editor.subjectId = e.detail?.value || '';
    });
    return;
  }

  host.innerHTML = `<tf-combobox id="tu-q-subject" label="${escapeAttr(T('field_subject'))}"
    placeholder="${escapeAttr(T('field_subject_hint'))}"></tf-combobox>`;
  const box = host.querySelector('#tu-q-subject');
  if (box) {
    box.options = options;
    if (editor.subjectId) box.value = editor.subjectId;
    box.addEventListener('change', (e) => {
      editor.subjectId = e.detail?.value || '';
    });
  }
}

async function saveQuota(modal, editor) {
  const body = modal.querySelector('[slot="body"]');
  const maxTokens = Number(body.querySelector('#tu-q-max')?.value || 0);
  const period = body.querySelector('#tu-q-period')?.value || 'daily';
  const isActive = !!body.querySelector('#tu-q-active')?.checked;

  if (editor.scopeType !== 'org' && !editor.subjectId) {
    toast(T('subject_required'), 'error');
    return;
  }
  if (!Number.isFinite(maxTokens) || maxTokens <= 0) {
    toast(T('max_tokens_invalid'), 'error');
    return;
  }

  const quota = {
    id: editor.quotaId,
    scopeType: editor.scopeType,
    subjectId: editor.scopeType === 'org' ? null : editor.subjectId,
    modelId: editor.modelId || null,
    period,
    maxTotalTokens: maxTokens,
    isActive,
  };

  try {
    await ApiBinary.one('tokenUpsertQuotaRequest', { quota });
    toast(T('saved'), 'success');
    closeModal(modal);
    await loadQuotas();
  } catch (err) {
    toast(err.message || T('save_failed'), 'error');
  }
}

async function confirmDeleteQuota(quota) {
  const choice = await TfModalConfirm(T('delete_title'), T('delete_confirm'));
  if (!choice) return;
  try {
    await ApiBinary.one('tokenDeleteQuotaRequest', { id: quota.id });
    toast(T('deleted'), 'success');
    await loadQuotas();
  } catch (err) {
    toast(err.message || T('delete_failed'), 'error');
  }
}

function TfModalConfirm(title, message) {
  return import('/js/components/tf-modal.js').then(({ default: TfModal }) =>
    TfModal.open({
      title,
      body: message,
      actions: [
        { label: T('cancel'), value: false },
        { label: T('delete'), value: true, primary: true },
      ],
    }),
  );
}

function closeModal(modal) {
  modal.removeAttribute('open');
  setTimeout(() => modal.remove(), 300);
}

// ---------------------------------------------------------------------------
// Zakładka "Koordynator".
// ---------------------------------------------------------------------------

function renderCoordinatorPanel() {
  const panel = byId('tu-panel');
  if (!panel) return;
  panel.innerHTML = `
    <section class="card">
      <div class="tu-coord-head">
        <span class="tf-label">${escapeHtml(T('coordinator_label'))}</span>
        <span id="tu-coord-node"></span>
      </div>
    </section>
    <section class="card" style="padding: 0; overflow: hidden;">
      <tf-table id="tu-leases-table">
        <tf-column key="quota" label="${escapeAttr(T('lease_quota'))}"></tf-column>
        <tf-column key="node" label="${escapeAttr(T('lease_node'))}"></tf-column>
        <tf-column key="period" label="${escapeAttr(T('lease_period'))}"></tf-column>
        <tf-column key="base" label="${escapeAttr(T('lease_base_used'))}" renderer="num"></tf-column>
        <tf-column key="granted" label="${escapeAttr(T('lease_granted'))}" renderer="num"></tf-column>
        <tf-column key="expires" label="${escapeAttr(T('lease_expires'))}"></tf-column>
      </tf-table>
    </section>
  `;
}

async function loadCoordinator() {
  try {
    const resp = await ApiBinary.one('tokenCoordinatorStatusRequest');
    coordinator = {
      nodeId: resp.coordinator_node_id ?? resp.coordinatorNodeId ?? null,
      leases: Array.isArray(resp.leases) ? resp.leases : [],
    };
    renderCoordinator();
  } catch (err) {
    coordinator = { nodeId: null, leases: [] };
    renderCoordinator();
    toast(err.message || T('load_failed'), 'error');
  }
}

function renderCoordinator() {
  const nodeHost = byId('tu-coord-node');
  if (nodeHost) {
    if (coordinator.nodeId) {
      nodeHost.innerHTML = `<tf-chip variant="info">${escapeHtml(coordinator.nodeId)}</tf-chip>`;
    } else {
      nodeHost.innerHTML = `<span class="muted">${escapeHtml(T('coordinator_none'))}</span>`;
    }
  }
  const table = byId('tu-leases-table');
  if (table) {
    table.rows = coordinator.leases.map((l) => ({
      quota: l.quota_id ?? l.quotaId ?? '',
      node: l.node_id ?? l.nodeId ?? '',
      period: l.period_key ?? l.periodKey ?? '',
      base: Number(l.base_used ?? l.baseUsed ?? 0),
      granted: Number(l.granted_tokens ?? l.grantedTokens ?? 0),
      expires: formatIso(l.expires_at ?? l.expiresAt),
    }));
  }
}

function formatIso(value) {
  if (!value) return '—';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(I18n.getLanguage());
}

export default TokenUsageScreen;
