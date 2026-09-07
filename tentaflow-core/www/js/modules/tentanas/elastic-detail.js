// =============================================================================
// Plik: modules/tentanas/elastic-detail.js
// Opis: Karta i szczegóły Elastic Array z odczytem stanu oraz odtworzeniem montowań.
// Przykład: drawElasticDetail(screen, body) korzysta z nazwy screen.array.
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, fmtOptionalBytes, fmtDate, errMessage, healthClass, POLL_POOLS_MS, ADMIN_TIMEOUT_MS } from '/js/modules/tentanas/format.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-breadcrumb.js';

const knownBytes = (value) => value != null && Number.isFinite(Number(value)) && Number(value) >= 0;
const row = (label, value) => `<div class="sr"><span class="k">${escapeHtml(label)}</span><span class="v">${escapeHtml(value)}</span></div>`;
const triState = (value) => value === true ? T('elastic.yes') : value === false ? T('elastic.no') : T('elastic.unknown');

export function elasticState(array) {
  const labels = { active: T('elastic.active'), pending: T('elastic.pending'), creating: T('elastic.pending'), needs_attention: T('elastic.error'), error: T('elastic.error'), disabled: T('elastic.disabled'), unknown: T('elastic.unknown') };
  return { label: labels[array.state] || labels.unknown, tone: array.state === 'active' ? 'ok' : ['error', 'needs_attention'].includes(array.state) ? 'err' : 'warn' };
}

function protectionLabel(array) {
  if (!(array.parityDisks || []).length) return T('elastic.no_parity');
  const labels = { protected: T('elastic.protected'), window_open: T('elastic.window_open'), unprotected: T('elastic.unprotected'), unknown: T('elastic.unknown') };
  return labels[array.protection?.status] || labels.unknown;
}

export function elasticCapacity(array) {
  const parity = array.parityDisks || [];
  const parityBytes = parity.every((d) => knownBytes(d.sizeBytes)) ? parity.reduce((n, d) => n + Number(d.sizeBytes), 0) : null;
  const measured = knownBytes(array.usableBytes) && knownBytes(array.usedBytes) && Number(array.usedBytes) <= Number(array.usableBytes);
  const free = measured ? Number(array.usableBytes) - Number(array.usedBytes) : null;
  const raw = measured && parityBytes != null ? Number(array.usableBytes) + parityBytes : null;
  return { measured, free, parityBytes, raw };
}

export function elasticCardHtml(array) {
  const state = elasticState(array);
  const c = elasticCapacity(array);
  const widths = c.raw > 0 ? [Number(array.usedBytes), c.free, c.parityBytes].map((n) => n / c.raw * 100) : null;
  return `<div class="pool-card nas-elastic-card" data-array="${escapeAttr(array.name)}">
    <div class="pc-head">
      <div class="pc-ico">${sprite('cylinder')}</div>
      <div class="pc-meta"><span class="pc-name">${escapeHtml(array.name)}</span>
        <tf-chip status="${state.tone}" dot label="${escapeAttr(state.label)}"></tf-chip>
        <tf-chip status="accent" label="Elastic Array"></tf-chip>
        <div class="pc-desc">${escapeHtml(T('elastic.topology', { data: (array.dataDisks || []).length, parity: (array.parityDisks || []).length, fs: array.filesystem.toUpperCase() }))}</div>
      </div>
      <div class="pc-actions"><tf-button variant="secondary" size="sm" icon="external-link" data-act="array-details">${escapeHtml(T('elastic.details'))}</tf-button></div>
    </div>
    <div class="pc-body">
      <div><div class="pc-cap"><span>${escapeHtml(T('elastic.capacity'))}</span><span class="v">${escapeHtml(fmtOptionalBytes(array.usedBytes))} / ${escapeHtml(fmtOptionalBytes(array.usableBytes))}</span></div>
        <div class="split-bar ${widths ? '' : 'nas-unmeasured'}" aria-label="${escapeAttr(widths ? T('elastic.capacity') : T('elastic.unmeasured'))}">${widths ? widths.map((w, i) => `<span class="${['data', 'free', 'parity'][i]}" style="width:${w}%"></span>`).join('') : ''}</div>
        <div class="legend-rows mt-sm">
          <div class="lr"><span class="sw data"></span>${escapeHtml(T('elastic.used'))}<span class="v">${escapeHtml(fmtOptionalBytes(array.usedBytes))}</span></div>
          <div class="lr"><span class="sw free"></span>${escapeHtml(T('elastic.free'))}<span class="v">${escapeHtml(fmtOptionalBytes(c.free))}</span></div>
          <div class="lr"><span class="sw parity"></span>${escapeHtml(T('elastic.parity'))}<span class="v">${escapeHtml(fmtOptionalBytes(c.parityBytes))}</span></div>
        </div>${!widths ? `<div class="hint">${escapeHtml(T('elastic.unmeasured'))}</div>` : ''}
      </div>
      <div class="stat-rows">${row(T('elastic.mountpoint'), array.unionPath || '—')}${row(T('elastic.last_sync'), fmtDate(array.protection?.protectedAsOf))}${row(T('elastic.protection'), protectionLabel(array))}</div>
    </div>
    ${array.stateDetail ? `<div class="pc-reason">${escapeHtml(array.stateDetail)}</div>` : ''}
  </div>`;
}

function diskHtml(disk, filesystem) {
  return `<div class="disk-cell" data-disk="${escapeAttr(disk.diskId)}">
    <span class="health-dot ${healthClass(disk.health)}"></span>
    <div class="dc-main"><div class="dc-name"><span class="mono">${escapeHtml(disk.name)}</span></div>
      <div class="dc-sub">${escapeHtml(fmtOptionalBytes(disk.usedBytes))} / ${escapeHtml(fmtOptionalBytes(disk.sizeBytes))} · ${escapeHtml(filesystem.toUpperCase())}</div>
      <div class="dc-sub">${escapeHtml(disk.device || disk.diskId)}</div>
      <div class="dc-sub">${escapeHtml(T('elastic.mounted'))}: ${escapeHtml(triState(disk.mounted))} · ${escapeHtml(T('elastic.present'))}: ${escapeHtml(triState(disk.devicePresent))}</div>
      <div class="dc-sub mono">${escapeHtml(disk.mountpoint)}</div>
    </div><tf-button variant="ghost" size="sm" icon="external-link" data-act="disk" title="${escapeAttr(T('elastic.disk_details', { name: disk.name }))}"></tf-button>
  </div>`;
}

export async function drawElasticDetail(screen, body) {
  const name = screen.array;
  const sourceNodeId = screen.currentNode()?.nodeId;
  const view = document.createElement('div');
  view.className = 'stack nas-elastic-detail';
  body.replaceChildren(view);
  const isCurrent = () => !screen.disposed && view.isConnected && screen.currentNode()?.nodeId === sourceNodeId && screen.array === name;
  let epoch = 0;
  let array = null;
  let busy = false;
  let submitted = false;
  let message = '';

  const draw = (error = '') => {
    if (!isCurrent()) return;
    const status = array && elasticState(array);
    const canRestore = array && !['active', 'creating'].includes(array.state) && array.enabled;
    view.innerHTML = `<tf-breadcrumb class="nas-crumbs"><tf-breadcrumb-item href="#">${escapeHtml(T('tabs.pools'))}</tf-breadcrumb-item><tf-breadcrumb-item current>${escapeHtml(name)}</tf-breadcrumb-item></tf-breadcrumb><div class="section-card-head nas-elastic-heading"><div class="title">${sprite('layers')} <span class="mono">${escapeHtml(name)}</span> <tf-chip status="accent" label="Elastic Array"></tf-chip></div><div class="actions">
      <tf-button variant="ghost" data-act="back">${escapeHtml(T('elastic.back'))}</tf-button><tf-button variant="secondary" icon="refresh" data-act="refresh">${escapeHtml(T('elastic.refresh'))}</tf-button></div></div>
      ${error ? `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(error)}"></tf-alert>` : ''}
      ${array ? `<div class="kpi">
        <tf-stat-card icon="cylinder" label="${escapeAttr(T('elastic.capacity'))}" value="${escapeAttr(fmtOptionalBytes(array.usedBytes))}" suffix="${escapeAttr('/ ' + fmtOptionalBytes(array.usableBytes))}"></tf-stat-card>
        <tf-stat-card icon="shield" label="${escapeAttr(T('elastic.protection'))}" value="${escapeAttr(protectionLabel(array))}" delta="${escapeAttr(T('elastic.last_sync') + ': ' + fmtDate(array.protection?.protectedAsOf))}"></tf-stat-card>
        <tf-stat-card icon="database" label="${escapeAttr(T('elastic.parity'))}" value="${(array.parityDisks || []).length}" delta="${escapeAttr(T('elastic.tolerance', { n: array.protection?.faultTolerance ?? '—' }))}"></tf-stat-card>
      </div>
      <div class="section-card"><div class="section-card-head"><div class="title">${sprite('cylinder')} ${escapeHtml(T('elastic.disks'))}</div><span class="hint">${escapeHtml(T('elastic.independent_fs'))}</span></div>
        <div class="vdev-group"><div class="vg-head"><span class="vg-type">${escapeHtml(T('elastic.data'))} · MERGERFS</span><span class="mono">${escapeHtml(array.unionPath)}</span><span class="hint">${escapeHtml(T('elastic.policy'))}: ${escapeHtml(array.createPolicy)}</span></div>
          <div class="disk-cells">${(array.dataDisks || []).map((d) => diskHtml(d, d.filesystem || array.filesystem)).join('')}</div></div>
        <div class="vdev-group"><div class="vg-head"><span class="vg-type">PARITY · SNAPRAID</span></div><div class="disk-cells">${(array.parityDisks || []).map((d) => diskHtml(d, array.filesystem)).join('')}</div>${!(array.parityDisks || []).length ? `<div class="hint">${escapeHtml(T('elastic.no_parity'))}</div>` : ''}</div>
      </div>
      <div class="grid-2"><div class="section-card"><div class="section-card-head"><div class="title">${sprite('shield')} ${escapeHtml(T('elastic.state'))}</div><tf-chip status="${status.tone}" dot label="${escapeAttr(status.label)}"></tf-chip></div>
        <div class="stat-rows">${row(T('elastic.mountpoint'), array.unionPath)}${row(T('elastic.state'), array.stateDetail || status.label)}${row(T('elastic.unprotected_bytes'), fmtOptionalBytes(array.protection?.movedUnsyncedBytes))}${row(T('elastic.updated'), fmtDate(array.updatedAt))}</div>
        <div class="explain-box mt-md">${escapeHtml(T('elastic.restore_hint'))}</div>
        ${screen.isAdmin && canRestore ? `<tf-button variant="secondary" class="mt-md" data-act="restore" ${busy || submitted ? 'disabled' : ''}>${escapeHtml(T('elastic.restore'))}</tf-button>` : ''}
        ${!screen.isAdmin ? `<div class="hint mt-sm">${escapeHtml(T('elevation.admin_only'))}</div>` : ''}
        ${message ? `<div class="explain-box mt-md" role="status">${escapeHtml(message)}</div><tf-button variant="ghost" data-act="jobs">${escapeHtml(T('elastic.jobs'))}</tf-button>` : ''}
      </div><div class="section-card"><div class="section-card-head"><div class="title">${sprite('shield')} SnapRAID</div></div><div class="stat-rows">
        ${row(T('elastic.last_sync'), fmtDate(array.protection?.protectedAsOf))}${row(T('elastic.parity_errors'), array.snapraid?.parityErrors ?? '—')}${row(T('elastic.config'), array.snapraid?.configPath || '—')}
      </div><div class="explain-box mt-md">${escapeHtml(T('elastic.snapshot_only'))}</div><div class="hint mt-sm">${escapeHtml(T('elastic.unmeasured'))}</div></div></div>` : error ? '' : `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`}`;
    view.querySelector('[data-act="back"]').addEventListener('click', () => { if (isCurrent()) screen.openArray(null); });
    view.querySelector('.nas-crumbs').addEventListener('click', (event) => {
      if (!event.target.closest('a')) return;
      event.preventDefault();
      if (isCurrent()) screen.openArray(null);
    });
    view.querySelector('[data-act="refresh"]').addEventListener('click', refresh);
    view.querySelector('[data-act="jobs"]')?.addEventListener('click', () => { if (isCurrent()) screen.switchTab('jobs'); });
    view.querySelectorAll('[data-act="disk"]').forEach((button) => {
      button.querySelector('button')?.setAttribute('aria-label', button.title);
      button.addEventListener('click', () => { if (isCurrent()) screen.openDisk(button.closest('[data-disk]').dataset.disk); });
    });
    view.querySelector('[data-act="restore"]')?.addEventListener('click', restore);
  };

  const refresh = async () => {
    if (!isCurrent()) return;
    const request = ++epoch;
    try {
      const result = await screen.nas('tentaNasElasticArrayGetRequest', { name });
      if (!isCurrent() || request !== epoch) return;
      if (!result.array || result.array.name !== name || result.array.kind !== 'elastic-array') throw new Error(T('elastic.bad_response'));
      array = result.array;
      draw();
    } catch (error) {
      if (isCurrent() && request === epoch) { array = null; draw(errMessage(error)); }
    }
  };

  const restore = async () => {
    if (!isCurrent() || !screen.isAdmin || busy || submitted) return;
    busy = true;
    draw();
    let sent = false;
    try {
      const result = await screen.withSudo((sudoPassword) => {
        if (!isCurrent()) return null;
        sent = true;
        submitted = true;
        return screen.nas('tentaNasElasticArrayRestoreRequest', { name, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS });
      }, T('elastic.restore'), isCurrent);
      if (!isCurrent()) return;
      if (result?.job?.jobId) {
        message = T('elastic.job_running');
        screen.openJobLog(result.job.jobId, () => { if (isCurrent()) { message = T('elastic.job_finished'); refresh(); } });
      } else if (result?.approval?.requestId) message = T('elastic.approval');
      else if (sent) message = T('elastic.request_unknown');
    } catch (error) {
      if (isCurrent()) message = sent ? T('elastic.request_unknown') : errMessage(error);
    } finally {
      busy = false;
      if (isCurrent()) { draw(); if (sent) await refresh(); }
    }
  };

  const poll = async () => { await refresh(); if (isCurrent()) screen.later(poll, POLL_POOLS_MS); };
  draw();
  await poll();
}
