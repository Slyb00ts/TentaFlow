// =============================================================================
// File: modules/addons/bindings.js
// Description: Bindings tab for addon detail. Shows the AI aliases owned by
//              this addon (readonly — full management lives in M16
//              Services -> Aliases), a vector-backend picker (zvec default /
//              Milvus with local service discovery + manual URL) backed by
//              AddonVectorBody, plus four storage usage cards (KV, SQL,
//              Vector, Recording) populated from AddonStorageStatsRequest.
//              Vector/Recording cards report "unavailable" when the build lacks
//              the vector/camera feature. Alias list still uses the addon-prefix
//              heuristic until ModelAliasEntry exposes owner_addon_id.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast, formatBytes } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';

let currentAddonId = null;
let currentContainer = null;
let aliases = [];
let storageStats = null;  // AddonStorageStatsResponse (kv/sql/vector/recording)
let vectorConfig = null;  // AddonVectorConfigResponse (backend + milvus discovery)

// i18n helper scoped to this tab's namespace.
const t = (k) => I18n.t('addon_bindings.' + k);

export const BindingsTab = {
  async mount(container, addonId) {
    currentAddonId = addonId;
    currentContainer = container;
    container.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
    await Promise.all([loadAliases(), loadStorageStats(), loadVectorConfig()]);
    render();
  },

  unmount() {
    currentAddonId = null;
    currentContainer = null;
    aliases = [];
    storageStats = null;
    vectorConfig = null;
  },
};

// Konfiguracja vector backendu addona (zvec / Milvus) + lista lokalnych serwisow
// Milvus do pickera. Admin endpoint AddonVectorGetConfigRequest.
async function loadVectorConfig() {
  try {
    vectorConfig = await ApiBinary.one('addonVectorGetConfigRequest', { addonId: currentAddonId });
  } catch (err) {
    vectorConfig = null;
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Statystyki storage addona (KV/SQL/Vector/Recording) — admin endpoint
// AddonStorageStatsRequest. SQL liczone z osobnego read-only polaczenia (nie
// blokuje zywego addona), liczby wierszy z capem.
async function loadStorageStats() {
  try {
    storageStats = await ApiBinary.one('addonStorageStatsRequest', { addonId: currentAddonId });
  } catch (err) {
    storageStats = null;
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Liczba z separatorem + sufiks "+" gdy capped.
function fmtCount(n, capped = false) {
  const v = Number(n);
  if (!Number.isFinite(v) || v < 0) return '—';
  return v.toLocaleString() + (capped ? '+' : '');
}

// NOTE: prefix heuristic is a STAND-IN until ModelAliasEntry exposes
// owner_addon_id (see backend-todo in CHANGELOG). False positive risk:
// alias 'teams-spy' would appear under addon 'teams' even if owned by
// another addon. Admin should verify ownership via M16 Aliasy page.
// This view is READ-ONLY — no destructive action possible on misattributed alias.
async function loadAliases() {
  try {
    const list = await ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' });
    const prefix = String(currentAddonId || '').toLowerCase();
    aliases = list.filter((a) => {
      const name = String(a.alias || '').toLowerCase();
      return prefix && (name === prefix || name.startsWith(prefix + '-') || name.startsWith(prefix + '_'));
    });
  } catch (err) {
    aliases = [];
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

function render() {
  if (!currentContainer) return;
  currentContainer.innerHTML = `
    <div class="addon-bindings">
      ${renderAliasesSection()}
      ${renderVectorBackendSection()}
      ${renderStorageSection()}
    </div>
  `;
  currentContainer.querySelector('#bindings-open-m16')?.addEventListener('click', () => {
    Router.navigate('services', { tab: 'aliases' });
  });
  currentContainer.querySelector('#bindings-refresh')?.addEventListener('click', async () => {
    await Promise.all([loadAliases(), loadStorageStats(), loadVectorConfig()]);
    render();
  });
  wireVectorBackendSection();
}

function renderAliasesSection() {
  const head = `
    <div class="section-card-head">
      <div class="title">
        <svg class="icon"><use href="#i-brain"/></svg>
        ${escapeHtml(I18n.t('addon_bindings.aliases_title'))}
        <span class="muted">· ${aliases.length}</span>
      </div>
      <div class="actions">
        <tf-button variant="ghost" icon="refresh" id="bindings-refresh">${escapeHtml(I18n.t('addon_bindings.refresh'))}</tf-button>
        <tf-button variant="secondary" icon="external-link" id="bindings-open-m16">${escapeHtml(I18n.t('addon_bindings.open_in_m16'))}</tf-button>
      </div>
    </div>
  `;

  const info = `
    <div class="bindings-info">
      <svg class="icon"><use href="#i-info"/></svg>
      <span>${escapeHtml(I18n.t('addon_bindings.readonly_note'))}</span>
    </div>
  `;

  if (aliases.length === 0) {
    return `
      <section class="section-card">
        ${head}
        ${info}
        <div class="addons-empty">${escapeHtml(I18n.t('addon_bindings.no_aliases'))}</div>
      </section>
    `;
  }

  const heuristicBanner = `
    <div class="bindings-info bindings-info-warn">
      <tf-chip status="warn">${escapeHtml(I18n.t('addon_bindings.heuristic_chip'))}</tf-chip>
      <span>${escapeHtml(I18n.t('addon_bindings.heuristic_note'))}</span>
    </div>
  `;

  const rows = aliases.map((a) => {
    const active = !!a.is_active;
    const statusLabel = active
      ? I18n.t('addon_bindings.status_active')
      : I18n.t('addon_bindings.status_inactive');
    const statusVariant = active ? 'ok' : 'warn';
    const target = String(a.target_model || '').trim();
    const fallback = String(a.fallback_targets || '').trim();
    const strategy = String(a.strategy || 'first_available');

    return `
      <tr>
        <td>
          <div class="alias-name">${escapeHtml(a.alias || '')}</div>
        </td>
        <td>
          <div class="alias-target">${target ? escapeHtml(target) : `<span class="muted">${escapeHtml(I18n.t('addon_bindings.no_target'))}</span>`}</div>
          ${fallback ? `<div class="alias-fallback">${escapeHtml(I18n.t('addon_bindings.fallback_chain'))}: ${escapeHtml(fallback)}</div>` : ''}
        </td>
        <td><tf-chip>${escapeHtml(strategy)}</tf-chip></td>
        <td><tf-chip status="${escapeAttr(statusVariant)}">${escapeHtml(statusLabel)}</tf-chip></td>
      </tr>
    `;
  }).join('');

  return `
    <section class="section-card">
      ${head}
      ${info}
      ${heuristicBanner}
      <!-- Raw <table class="tf-table"> is a project-wide class-only convention
           (see logs.js, profiling-sessions.js). The <tf-table> component
           expects array-driven .rows + <tf-column> children; this view stays
           on the convention to match its siblings. -->
      <table class="tf-table bindings-alias-table">
        <thead>
          <tr>
            <th>${escapeHtml(I18n.t('addon_bindings.col_alias'))}</th>
            <th>${escapeHtml(I18n.t('addon_bindings.col_target'))}</th>
            <th>${escapeHtml(I18n.t('addon_bindings.col_strategy'))}</th>
            <th>${escapeHtml(I18n.t('addon_bindings.col_status'))}</th>
          </tr>
        </thead>
        <tbody>${rows}</tbody>
      </table>
    </section>
  `;
}

// Sekcja wyboru vector backendu: zvec (domyslny, wbudowany) vs Milvus. Dla
// Milvus — wybor lokalnego serwisu (discovery) albo reczny URL + sekrety.
// Milvus jest wylaczony gdy build nie ma feature vector-milvus.
function renderVectorBackendSection() {
  const v = vectorConfig;
  if (!v) {
    return `
      <section class="section-card">
        <div class="section-card-head">
          <div class="title"><svg class="icon"><use href="#i-cpu"/></svg>${escapeHtml(t('vcfg_title'))}</div>
        </div>
        <div class="addons-empty">${escapeHtml(t('stats_unavailable'))}</div>
      </section>
    `;
  }

  const cfg = v.config || { backend: 'zvec' };
  const milvusOk = !!v.milvusCompiled;
  // Build bez vector-milvus nie moze pokazac/zapisac milvus — wymuszamy zvec.
  const backend = cfg.backend === 'milvus' && milvusOk ? 'milvus' : 'zvec';
  // Tylko osiagalne serwisy sa wybieralne — niedostepny serwis nie jest stanem
  // konfiguracyjnym. Composite value `nodeId|serviceId` jest forward-compatible
  // z C-2 (zdalne serwisy); dla lokalnego slice'a nodeId jest puste.
  const allServices = Array.isArray(v.milvusServices) ? v.milvusServices : [];
  const services = allServices.filter((s) => s.reachable);
  const source = cfg.milvusSource === 'manual'
    ? 'manual'
    : (cfg.milvusSource === 'service_ref' || services.length ? 'service_ref' : 'manual');
  const selRef = cfg.serviceRef || {};
  const selectedKey = `${selRef.nodeId || ''}|${selRef.serviceId || ''}`;
  const manualUri = cfg.manualUri || '';
  const collection = cfg.collectionOverride || '';
  const hasSecret = !!v.hasMilvusUser || !!v.hasMilvusPassword;

  const milvusWarn = milvusOk ? '' : `
    <div class="bindings-info bindings-info-warn">
      <svg class="icon"><use href="#i-info"/></svg>
      <span>${escapeHtml(t('vcfg_milvus_unavailable'))}</span>
    </div>
  `;

  const serviceOptions = services.map((s) => {
    const key = `${s.nodeId || ''}|${s.serviceId}`;
    const local = s.local ? ` · ${t('vcfg_local_chip')}` : '';
    const label = `${s.displayName || s.serviceId}${local}`;
    return `<option value="${escapeAttr(key)}" ${key === selectedKey ? 'selected' : ''}>${escapeHtml(label)}</option>`;
  }).join('');

  const serviceBlock = services.length
    ? `<tf-select id="vcfg-service" value="${escapeAttr(selectedKey)}">${serviceOptions}</tf-select>`
    : `<div class="bindings-info"><svg class="icon"><use href="#i-info"/></svg><span>${escapeHtml(t('vcfg_no_services'))}</span></div>`;

  const clearSecretsRow = hasSecret
    ? `<tf-checkbox id="vcfg-clear-secrets" label="${escapeAttr(t('vcfg_clear_secrets'))}"></tf-checkbox>`
    : '';

  const milvusBlock = `
    <div class="vcfg-milvus" id="vcfg-milvus" style="${backend === 'milvus' ? '' : 'display:none'}">
      <tf-radio-group name="vcfg-source" id="vcfg-source" value="${escapeAttr(source)}">
        <tf-radio value="service_ref" label="${escapeAttr(t('vcfg_source_service'))}"></tf-radio>
        <tf-radio value="manual" label="${escapeAttr(t('vcfg_source_manual'))}"></tf-radio>
      </tf-radio-group>
      <div class="vcfg-source-block" id="vcfg-block-service" style="${source === 'service_ref' ? '' : 'display:none'}">
        <span class="tf-label">${escapeHtml(t('vcfg_service_label'))}</span>
        ${serviceBlock}
      </div>
      <div class="vcfg-source-block" id="vcfg-block-manual" style="${source === 'manual' ? '' : 'display:none'}">
        <tf-input id="vcfg-uri" label="${escapeAttr(t('vcfg_manual_uri_label'))}" placeholder="http://host:19530" value="${escapeAttr(manualUri)}"></tf-input>
      </div>
      <tf-input id="vcfg-collection" label="${escapeAttr(t('vcfg_collection_label'))}" value="${escapeAttr(collection)}" maxlength="128"></tf-input>
      <tf-input id="vcfg-user" label="${escapeAttr(t('vcfg_user_label'))}" placeholder="${v.hasMilvusUser ? escapeAttr(t('vcfg_secret_set')) : ''}" maxlength="512" autocomplete="off"></tf-input>
      <tf-input id="vcfg-pass" type="password" label="${escapeAttr(t('vcfg_pass_label'))}" placeholder="${v.hasMilvusPassword ? escapeAttr(t('vcfg_secret_set')) : ''}" maxlength="512" autocomplete="new-password"></tf-input>
      ${clearSecretsRow}
    </div>
  `;

  return `
    <section class="section-card">
      <div class="section-card-head">
        <div class="title"><svg class="icon"><use href="#i-cpu"/></svg>${escapeHtml(t('vcfg_title'))}</div>
        <div class="actions">
          <tf-button variant="primary" icon="check" id="vcfg-save">${escapeHtml(t('vcfg_save'))}</tf-button>
        </div>
      </div>
      <div class="bindings-info"><svg class="icon"><use href="#i-info"/></svg><span>${escapeHtml(t('vcfg_desc'))}</span></div>
      <tf-radio-group name="vcfg-backend" id="vcfg-backend" value="${escapeAttr(backend)}">
        <tf-radio value="zvec" label="${escapeAttr(t('vcfg_backend_zvec'))}"></tf-radio>
        <tf-radio value="milvus" label="${escapeAttr(t('vcfg_backend_milvus'))}" ${milvusOk ? '' : 'disabled'}></tf-radio>
      </tf-radio-group>
      <div class="vcfg-hints">
        <div class="muted">${escapeHtml(t('vcfg_backend_zvec_sub'))}</div>
        <div class="muted">${escapeHtml(t('vcfg_backend_milvus_sub'))}</div>
      </div>
      ${milvusWarn}
      ${milvusBlock}
    </section>
  `;
}

// Interaktywnosc pickera bez re-renderu (zachowuje wpisane wartosci): radio
// backendu pokazuje/ukrywa blok Milvus, radio zrodla przelacza serwis/URL.
function wireVectorBackendSection() {
  const root = currentContainer;
  if (!root) return;
  const milvusBlock = root.querySelector('#vcfg-milvus');
  root.querySelector('#vcfg-backend')?.addEventListener('change', (e) => {
    if (milvusBlock) milvusBlock.style.display = e.detail?.value === 'milvus' ? '' : 'none';
  });
  root.querySelector('#vcfg-source')?.addEventListener('change', (e) => {
    const svc = root.querySelector('#vcfg-block-service');
    const man = root.querySelector('#vcfg-block-manual');
    const isService = e.detail?.value === 'service_ref';
    if (svc) svc.style.display = isService ? '' : 'none';
    if (man) man.style.display = isService ? 'none' : '';
  });
  root.querySelector('#vcfg-save')?.addEventListener('click', saveVectorConfig);
}

// Zbiera wartosci z DOM i zapisuje przez AddonVectorSetConfigRequest. Puste
// pola sekretow oznaczaja "nie zmieniaj" (None na wire), wiec ich nie wysylamy.
async function saveVectorConfig() {
  const root = currentContainer;
  if (!root) return;
  const backend = root.querySelector('#vcfg-backend')?.value || 'zvec';
  const payload = { addonId: currentAddonId, backend };

  if (backend === 'milvus') {
    const srcSel = root.querySelector('#vcfg-source')?.value || 'service_ref';
    payload.milvusSource = srcSel;
    if (srcSel === 'service_ref') {
      // Composite `nodeId|serviceId` — rozbij na osobne pola wire.
      const key = root.querySelector('#vcfg-service')?.value || '';
      const sep = key.indexOf('|');
      const serviceId = sep >= 0 ? key.slice(sep + 1) : key;
      if (!serviceId) {
        toast(t('vcfg_no_services'), 'error');
        return;
      }
      payload.serviceNodeId = sep >= 0 ? key.slice(0, sep) : '';
      payload.serviceId = serviceId;
    } else {
      const uri = (root.querySelector('#vcfg-uri')?.value || '').trim();
      if (!uri) {
        toast(t('vcfg_manual_uri_label'), 'error');
        return;
      }
      payload.manualUri = uri;
    }
    // Pole edytowalne — wyslij zawsze (pusty string czysci nadpisanie).
    payload.collectionOverride = (root.querySelector('#vcfg-collection')?.value || '').trim();
    // Sekrety: domyslnie puste = "nie zmieniaj" (pomijamy). Checkbox "wyczyść"
    // jawnie wysyla pusty string = usuniecie zapisanego sekretu.
    const clearSecrets = !!root.querySelector('#vcfg-clear-secrets')?.checked;
    const user = (root.querySelector('#vcfg-user')?.value || '').trim();
    const pass = (root.querySelector('#vcfg-pass')?.value || '').trim();
    if (clearSecrets) {
      payload.milvusUser = '';
      payload.milvusPassword = '';
    } else {
      if (user) payload.milvusUser = user;
      if (pass) payload.milvusPassword = pass;
    }
  }

  try {
    const res = await ApiBinary.one('addonVectorSetConfigRequest', payload);
    if (res && res.ok === false) throw new Error(res.error || I18n.t('common.error'));
    toast(t('vcfg_saved'), 'success');
    await Promise.all([loadVectorConfig(), loadStorageStats()]);
    render();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

// Jeden kafelek storage: tytul, glowna wartosc, podtytul, opcjonalny pasek
// wypelnienia i chip-status. `unavailable` renderuje stan "—" + chip warn.
function storageCard({ icon, title, subtitle, value, fillPct = null, chip = null }) {
  const bar = fillPct != null
    ? `<div class="bar-thin"><div class="fill" style="width:${Math.max(0, Math.min(100, fillPct))}%"></div></div>`
    : '';
  const foot = chip
    ? `<div class="card-foot"><tf-chip status="${escapeAttr(chip.status)}">${escapeHtml(chip.label)}</tf-chip></div>`
    : '';
  const valCls = value === '—' ? 'v muted' : 'v';
  return `
    <div class="usage-card">
      <div class="h">
        <svg class="icon"><use href="#i-${escapeAttr(icon)}"/></svg>
        <span>${escapeHtml(title)}</span>
      </div>
      <div class="${valCls}">${escapeHtml(value)}</div>
      <div class="sub">${escapeHtml(subtitle)}</div>
      ${bar}
      ${foot}
    </div>
  `;
}

function renderStorageSection() {
  const s = storageStats;
  if (!s) {
    return `
      <section class="section-card">
        <div class="section-card-head">
          <div class="title"><svg class="icon"><use href="#i-database"/></svg>${escapeHtml(I18n.t('addon_bindings.storage_title'))}</div>
        </div>
        <div class="addons-empty">${escapeHtml(I18n.t('addon_bindings.stats_unavailable'))}</div>
      </section>
    `;
  }

  // KV
  const kv = s.kv || { keys: 0, bytes: 0, limitMb: 0 };
  const kvLimitMb = Number(kv.limitMb ?? kv.limit_mb ?? 0);
  const kvFill = kvLimitMb > 0 ? (Number(kv.bytes) / (kvLimitMb * 1024 * 1024)) * 100 : null;
  const kvCard = storageCard({
    icon: 'key',
    title: I18n.t('addon_bindings.storage_kv_title'),
    value: `${fmtCount(kv.keys)} kluczy`,
    subtitle: kvLimitMb > 0 ? `${formatBytes(kv.bytes)} / ${kvLimitMb} MB` : formatBytes(kv.bytes),
    fillPct: kvFill,
  });

  // SQL
  const sql = s.sql || {};
  const sqlSize = Number(sql.dbSizeBytes ?? sql.db_size_bytes ?? -1);
  let sqlCard;
  if (!sql.enabled) {
    sqlCard = storageCard({
      icon: 'database',
      title: I18n.t('addon_bindings.storage_sql_title'),
      value: '—',
      subtitle: I18n.t('addon_bindings.storage_sql_sub'),
      chip: { status: 'info', label: 'Addon nie używa SQL' },
    });
  } else if (!sql.available) {
    sqlCard = storageCard({
      icon: 'database',
      title: I18n.t('addon_bindings.storage_sql_title'),
      value: '—',
      subtitle: I18n.t('addon_bindings.storage_sql_sub'),
      chip: { status: 'warn', label: I18n.t('addon_bindings.stats_unavailable') },
    });
  } else {
    const tables = Array.isArray(sql.tables) ? sql.tables : [];
    sqlCard = storageCard({
      icon: 'database',
      title: I18n.t('addon_bindings.storage_sql_title'),
      value: sqlSize >= 0 ? formatBytes(sqlSize) : '—',
      subtitle: `${tables.length} ${tables.length === 1 ? 'tabela' : 'tabel'}`,
    });
  }

  // Vector
  const vec = s.vector || {};
  let vecCard;
  if (!vec.available) {
    vecCard = storageCard({
      icon: 'cpu',
      title: I18n.t('addon_bindings.storage_vector_title'),
      value: '—',
      subtitle: I18n.t('addon_bindings.storage_vector_sub'),
      chip: { status: 'info', label: 'Niedostępne (build bez vector)' },
    });
  } else {
    const namespaces = Array.isArray(vec.namespaces) ? vec.namespaces : [];
    const totalVectors = namespaces.reduce((acc, n) => acc + Number(n.count || 0), 0);
    vecCard = storageCard({
      icon: 'cpu',
      title: I18n.t('addon_bindings.storage_vector_title'),
      value: `${fmtCount(totalVectors)} wekt.`,
      subtitle: `${namespaces.length} namespace`,
    });
  }

  // Recording
  const rec = s.recording || {};
  let recCard;
  if (!rec.available) {
    recCard = storageCard({
      icon: 'record',
      title: I18n.t('addon_bindings.storage_recording_title'),
      value: '—',
      subtitle: I18n.t('addon_bindings.storage_recording_sub'),
      chip: { status: 'info', label: 'Niedostępne (build bez camera)' },
    });
  } else {
    recCard = storageCard({
      icon: 'record',
      title: I18n.t('addon_bindings.storage_recording_title'),
      value: `${fmtCount(rec.segments)} segm. / ${fmtCount(rec.snapshots)} snap.`,
      subtitle: formatBytes(rec.bytes),
    });
  }

  return `
    <section class="section-card">
      <div class="section-card-head">
        <div class="title">
          <svg class="icon"><use href="#i-database"/></svg>
          ${escapeHtml(I18n.t('addon_bindings.storage_title'))}
        </div>
      </div>
      <div class="usage-grid">${kvCard}${sqlCard}${vecCard}${recCard}</div>
      ${renderStorageDetail(sql, vec)}
    </section>
  `;
}

// Szczegoly: lista tabel SQL (nazwa -> liczba wierszy) i namespace'ow wektorowych.
function renderStorageDetail(sql, vec) {
  const blocks = [];
  const tables = sql && sql.available && Array.isArray(sql.tables) ? sql.tables : [];
  if (tables.length > 0) {
    const rows = tables.map((t) => `
      <tr>
        <td>${escapeHtml(t.name)}</td>
        <td>${fmtCount(t.rows, !!(t.rowsCapped ?? t.rows_capped))} wierszy</td>
      </tr>`).join('');
    blocks.push(`
      <div class="storage-detail">
        <div class="storage-detail-title">Tabele SQL</div>
        <table class="tf-table"><tbody>${rows}</tbody></table>
      </div>`);
  }
  const namespaces = vec && vec.available && Array.isArray(vec.namespaces) ? vec.namespaces : [];
  if (namespaces.length > 0) {
    const rows = namespaces.map((n) => `
      <tr>
        <td>${escapeHtml(n.namespace)}</td>
        <td>${fmtCount(n.count)} wekt.</td>
        <td>dim ${escapeHtml(String(n.dim))} · ${escapeHtml(String(n.metric))}</td>
      </tr>`).join('');
    blocks.push(`
      <div class="storage-detail">
        <div class="storage-detail-title">Namespace'y wektorowe</div>
        <table class="tf-table"><tbody>${rows}</tbody></table>
      </div>`);
  }
  return blocks.join('');
}

export default BindingsTab;
