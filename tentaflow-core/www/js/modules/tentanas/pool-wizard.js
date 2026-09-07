// ===== File: modules/tentanas/pool-wizard.js — the "new pool" wizard (n07/n08): pool type → disks → layout and options → summary with retype, then the create job in place =====
//
// The window, header, progress rail and footer are the addon install wizard
// 1:1 (the same CSS classes) so a pool creation feels like every other
// multi-step flow of the dashboard. The layout step asks the node
// (`PoolPlanRequest`) for the candidate layouts of the picked disks so the
// usable capacity and fault tolerance shown are what `zpool create` will
// report — the frontend never computes RAIDZ maths itself.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import {
  T, sprite, POLL_JOB_MODAL_MS, ADMIN_TIMEOUT_MS,
  fmtBytes, pct, healthClass, errMessage, layoutLabel, jobKindLabel,
} from '/js/modules/tentanas/format.js';
import '/js/components/tf-window.js';
import '/js/components/tf-choice-card.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-table.js';
import '/js/components/tf-segmented.js';

// zpool(8) naming: a pool name starts with a letter and must not be one of
// the vdev keywords; the node re-checks, this only keeps the button honest.
const NAME_RE = /^[a-zA-Z][a-zA-Z0-9_.:-]*$/;
const RESERVED = new Set(['mirror', 'raidz', 'raidz1', 'raidz2', 'raidz3', 'draid', 'spare', 'log', 'cache', 'special', 'dedup']);
export const poolNameValid = (name) => NAME_RE.test(name) && !RESERVED.has(name.toLowerCase());
export const elasticNameValid = (name) => /^[a-zA-Z0-9][a-zA-Z0-9_.:-]{0,63}$/.test(name) && !['tentanas', 'tentanas-branches'].includes(name);

const COMPRESSION_OPTIONS = ['zstd', 'lz4', 'off'];

const KIND_LABELS = { zfs: 'ZFS', anyraid: 'ZFS AnyRAID', elastic: 'Elastic Array' };

/**
 * Every disk of the node for the picker: the free ones are selectable, the
 * members and spares of existing pools stay visible but disabled with the
 * reason on the cell (the mockup shows "why can't I pick sda" in place).
 */
export function wizardDisks(freeDisks, pools) {
  const rows = freeDisks.map((d) => ({ disk: d, reason: null }));
  for (const p of pools) {
    for (const v of p.vdevs || []) {
      for (const d of v.disks || []) {
        const spare = v.role === 'spare';
        rows.push({
          disk: { diskId: d.diskId, name: d.name, sizeBytes: d.sizeBytes, kind: '', serial: '', model: '', health: 'ok' },
          reason: spare
            ? { title: T('wizard_pool.spare_title', { pool: p.name }), sub: T('wizard_pool.spare_sub', { pool: p.name }) }
            : { title: T('wizard_pool.occupied_title', { pool: p.name, layout: v.kind }), sub: T('wizard_pool.occupied_sub', { pool: p.name }) },
        });
      }
    }
  }
  return rows;
}

/**
 * Opens the wizard on `screen` (the TentaNas screen: `nas`, `withSudo`,
 * `currentNode`, `environment`). `freeDisks` are the node's unassigned
 * disks and `pools` its pools from `PoolsListResponse`; `onDone(job)` runs
 * once the create job has finished.
 */
export function openPoolWizard(screen, { freeDisks = [], pools = [], onDone = null, onCreated = null, isCurrent = () => true } = {}) {
  if (screen.openWindow) { screen.openWindow.remove(); screen.openWindow = null; }
  const node = screen.currentNode();
  const sourceNodeId = screen.nodeId;
  const sourceTab = screen.tab;
  const sourceRoot = screen.root;
  const sourceActive = () => !screen.disposed && screen.nodeId === sourceNodeId && screen.currentNode()?.nodeId === node.nodeId && screen.tab === sourceTab && screen.root === sourceRoot && (!sourceRoot || sourceRoot.isConnected) && isCurrent();
  const zfsVersion = (screen.environment?.features || []).find((f) => f.id === 'zfs')?.version || node.zfsVersion || '—';
  const state = {
    step: 0,
    kind: 'zfs',
    diskIds: new Set(),
    plan: null,
    planError: '',
    layout: '',
    name: '',
    compression: 'zstd',
    encryption: false,
    confirm: '',
    job: null,
    result: null,
    timer: null,
    capabilities: null,
    capabilitiesError: '',
    elasticDisks: [],
    parityIds: new Set(),
    filesystem: '',
    revision: 0,
    planRevision: -1,
    planning: false,
    busy: false,
    submitted: false,
    outcome: null,
    notified: false,
    closed: false,
  };
  const steps = [T('wizard_pool.step_kind'), T('wizard_pool.step_disks'), T('wizard_pool.step_layout'), T('wizard_pool.step_summary')];
  const subs = [T('wizard_pool.sub_kind'), T('wizard_pool.sub_disks'), T('wizard_pool.header_sub_layout'), T('wizard_pool.sub_summary')];
  const allDisks = wizardDisks(freeDisks, pools);
  const diskById = new Map(freeDisks.map((d) => [d.diskId, d]));
  const selectedMap = () => state.kind === 'elastic' ? new Map(state.elasticDisks.map((d) => [d.diskId, d])) : diskById;
  const picked = () => [...state.diskIds].map((id) => selectedMap().get(id)).filter(Boolean);
  const parity = () => [...state.parityIds].map((id) => selectedMap().get(id)).filter(Boolean);
  const erased = () => state.kind === 'elastic' ? [...picked(), ...parity()] : picked();
  const elasticAvailable = () => state.capabilities?.mergerfs === true && state.capabilities.filesystems.some((fs) => fs === 'xfs' || fs === 'ext4');
  const invalidatePlan = () => { state.revision++; state.plan = null; state.planRevision = -1; state.planError = ''; state.confirm = ''; state.layout = ''; state.planning = false; };
  const draft = () => ({ name: state.name, filesystem: state.filesystem, dataDiskIds: [...state.diskIds], parityDiskIds: [...state.parityIds], cacheDiskIds: [] });
  // Capacity of a vdev follows its smallest member, so "2 × 8 TB" quotes
  // the smallest picked disk, exactly as the node's plan will.
  const smallestBytes = () => picked().reduce((a, d) => Math.min(a, Number(d.sizeBytes) || 0), Infinity);
  const selectedText = () => state.kind === 'elastic' ? picked().map((d) => `${d.name} (${fmtBytes(d.sizeBytes)})`).join(' + ') || '0' : (state.diskIds.size ? T('wizard_pool.selected_value', { n: state.diskIds.size, size: fmtBytes(smallestBytes()) }) : '0');
  const eraseText = () => {
    const serials = erased().map((d) => d.serial || d.name);
    return serials.length ? T('wizard_pool.erase_warning', { serials: serials.join(', ') }) : T('wizard_pool.erase_warning_none');
  };

  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('wizard_pool.title'));
  win.setAttribute('icon', 'layers');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '820');
  win.setAttribute('min-width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  screen.openWindow = win;
  const active = () => !state.closed && win.isConnected && sourceActive();
  const notifyCreated = () => {
    if (state.outcome && !state.notified && sourceActive()) {
      state.notified = true;
      onCreated?.({ kind: 'elastic', name: state.name, ...state.outcome });
    }
  };

  const header = () => {
    const elastic = state.kind === 'elastic';
    win.classList.toggle('nas-elastic-wizard', elastic);
    win.setAttribute('icon', elastic ? 'cylinder' : 'layers');
    win.setAttribute('width', elastic ? '760' : '820');
    win.setAttribute('min-width', String(Math.min(elastic ? 320 : 640, window.innerWidth - 24)));
    const labels = elastic ? [steps[0], T('wizard_pool.elastic_data'), T('wizard_pool.elastic_parity'), steps[3]] : steps;
    win.setAttribute('title', state.step > 0 ? T('wizard_pool.heading_kind', { kind: KIND_LABELS[state.kind] }) : T('wizard_pool.title'));
    return `
    <div class="install-header">
      <div class="big-ico">${sprite(elastic ? 'cylinder' : 'layers')}</div>
      <div class="install-header-meta">
        <h1>${escapeHtml(T('wizard_pool.heading'))} <span class="version">${escapeHtml(T('wizard.node_tag', { node: node.nodeName }))}</span></h1>
        <div class="sub">${escapeHtml(elastic && state.step === 1 ? T('wizard_pool.elastic_data_sub') : elastic && state.step === 2 ? T('wizard_pool.elastic_parity_sub') : subs[state.step])}</div>
      </div>
    </div>
    <div class="install-progress">${labels.map((s, i) => `<div class="install-step ${i === state.step ? 'active' : i < state.step ? 'done' : ''}"><span class="num">${i < state.step ? sprite('check') : i + 1}</span><span class="label">${escapeHtml(s)}</span></div>`).join('')}</div>`;
  };

  // Dostępność Elastic pochodzi z aktualnego węzła, nie z obecności ZFS.
  const stepKind = () => `
    <h2 class="wizard-section-title">${escapeHtml(T('wizard_pool.kind_title'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(T('wizard_pool.kind_sub'))}</p>
    <tf-choice-group id="nas-pw-kind" value="${escapeAttr(state.kind)}" columns="3">
      <tf-choice-card value="zfs" icon="layers" heading="ZFS" description="${escapeAttr(T('wizard_pool.kind_zfs_desc'))}"></tf-choice-card>
      <tf-choice-card value="anyraid" icon="layers" heading="ZFS AnyRAID" description="${escapeAttr(T('wizard_pool.kind_anyraid_desc'))}" title="${escapeAttr(T('wizard_pool.kind_anyraid_title', { v: zfsVersion }))}" disabled></tf-choice-card>
      <tf-choice-card value="elastic" icon="cylinder" heading="Elastic Array" description="${escapeAttr(T('wizard_pool.kind_elastic_desc'))}" ${elasticAvailable() ? '' : 'disabled'}></tf-choice-card>
    </tf-choice-group>
    <div class="text-xs text-3 mt-md">${escapeHtml(T('wizard_pool.kind_hint'))}</div>
    ${elasticAvailable() ? '' : `<div class="wizard-warning info mt-md">${sprite('info')}<div>${escapeHtml(state.capabilitiesError || state.capabilities?.detail || (state.capabilities ? T('wizard_pool.elastic_unavailable') : I18n.t('common.loading')))}<tf-button variant="ghost" data-pw-environment>${escapeHtml(T('tabs.environment'))}</tf-button></div></div>`}`;

  // Step 2 — disks. Members and spares of other pools are disabled with the
  // reason; a free disk with a critical SMART verdict cannot be picked either.
  const stepDisks = () => {
    const rows = state.kind === 'elastic' ? state.elasticDisks.map((disk) => ({ disk, reason: null })) : allDisks;
    const cells = rows.map(({ disk: d, reason }) => {
      const blocked = Boolean(reason) || d.health === 'critical';
      const on = state.diskIds.has(d.diskId);
      const title = reason ? reason.title : (d.healthReason || '');
      const sub = reason ? reason.sub : [[fmtBytes(d.sizeBytes), d.kind ? d.kind.toUpperCase() : ''].filter(Boolean).join(' '), d.serial || ''].filter(Boolean).join(' · ');
      return `
        <div class="disk-cell ${on ? 'checked' : ''} ${blocked ? 'disabled' : ''}" data-disk="${escapeAttr(d.diskId)}" ${title ? `title="${escapeAttr(title)}"` : ''}>
          <tf-checkbox ${on ? 'checked' : ''} ${blocked ? 'disabled' : ''}></tf-checkbox>
          <div class="dc-main">
            <div class="dc-name"><span class="health-dot ${healthClass(d.health)}"></span><span class="mono">${escapeHtml(d.name)}</span></div>
            <div class="dc-sub">${escapeHtml(sub)}</div>
          </div>
        </div>`;
    }).join('');
    return `
      <h2 class="wizard-section-title">${escapeHtml(state.kind === 'elastic' ? T('wizard_pool.elastic_data') : T('wizard_pool.disks_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(state.kind === 'elastic' ? T('wizard_pool.elastic_data_sub') : T('wizard_pool.disks_sub'))}</p>
      ${rows.length ? `<div class="disk-cells" id="nas-pw-disks">${cells}</div>` : `<div class="muted">${escapeHtml(T('pools.no_free_disks'))}</div>`}
      ${state.kind === 'elastic' ? `<div class="field mt-md"><label>${escapeHtml(T('wizard_pool.elastic_fs'))}</label><tf-segmented id="nas-pw-filesystem" aria-label="${escapeAttr(T('wizard_pool.elastic_fs'))}" value="${escapeAttr(state.filesystem)}">${state.capabilities.filesystems.filter((v) => ['xfs', 'ext4'].includes(v)).map((v) => `<option value="${v}">${v === 'xfs' ? 'XFS' : 'ext4'}</option>`).join('')}</tf-segmented><div class="hint">${escapeHtml(T('wizard_pool.elastic_fs_hint'))}</div></div>` : ''}
      <div class="mt-md"><span class="kv-inline"><span class="k">${escapeHtml(T('wizard_pool.selected'))}</span><span class="v mono" id="nas-pw-selected">${escapeHtml(selectedText())}</span></span></div>
      <div class="wizard-warning danger mt-md">${sprite('alert')}<div id="nas-pw-erase">${escapeHtml(eraseText())}</div></div>`;
  };

  const explainHtml = (chosen) => (chosen
    ? T('wizard_pool.layout_explain', {
      layout: escapeHtml(layoutLabel(chosen.layout)), n: state.diskIds.size, size: escapeHtml(fmtBytes(state.plan.smallestDiskBytes)),
      usable: escapeHtml(fmtBytes(chosen.usableBytes)), pct: pct(chosen.usableBytes, chosen.rawBytes), ft: chosen.faultTolerance,
    })
    : escapeHtml(T('wizard_pool.layout_pick')));

  // Step 3 — layout and options. Cards come from the node's plan; an
  // unavailable layout stays visible with the reason so "why no RAIDZ2" has
  // an answer on the screen.
  const stepLayout = () => {
    if (state.kind === 'elastic') return stepParity();
    const plan = state.plan;
    let cards = '';
    if (state.planError) {
      cards = `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(state.planError)}</div></div>`;
    } else if (!plan) {
      cards = `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`;
    } else {
      cards = `<tf-choice-group id="nas-pw-layout" value="${escapeAttr(state.layout)}" columns="2">${plan.options.map((o) => {
        let pill = '';
        if (!o.available) pill = `note="${escapeAttr(reasonLabel(o.reason))}" disabled`;
        else if (o.layout === 'stripe') pill = `pill="${escapeAttr(T('wizard_pool.no_redundancy'))}" pill-tone="err"`;
        else if (o.recommended) pill = `pill="${escapeAttr(T('wizard.recommended'))}" pill-tone="ok"`;
        return `
        <tf-choice-card value="${escapeAttr(o.layout)}" icon="${o.layout === 'stripe' ? 'alert' : 'shield'}" heading="${escapeAttr(layoutLabel(o.layout))}"
          description="${escapeAttr(o.available ? T('wizard_pool.layout_desc', { usable: fmtBytes(o.usableBytes), pct: pct(o.usableBytes, o.rawBytes), ft: o.faultTolerance }) : '')}" ${pill}></tf-choice-card>`;
      }).join('')}</tf-choice-group>`;
    }
    const chosen = plan && plan.options.find((o) => o.layout === state.layout);
    return `
      <h2 class="wizard-section-title">${escapeHtml(T('wizard_pool.layout_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('wizard_pool.sub_layout'))}</p>
      ${cards}
      ${(plan?.warnings || []).map((w) => `<div class="wizard-warning info mt-sm">${sprite('info')}<div>${escapeHtml(w)}</div></div>`).join('')}
      <div class="explain-box mt-md" id="nas-pw-explain">${explainHtml(chosen)}</div>
      <div class="form-grid-2 mt-md">
        <tf-input id="nas-pw-name" label="${escapeAttr(T('wizard_pool.name_label'))}" placeholder="tank" autocomplete="off" spellcheck="false" value="${escapeAttr(state.name)}"></tf-input>
        <tf-select id="nas-pw-compression" label="${escapeAttr(T('wizard_pool.compression_label'))}"></tf-select>
      </div>
      <div class="toggle-card mt-md">
        <div class="tc-text"><span>${escapeHtml(T('wizard_pool.encryption'))}</span><span class="tc-sub">${escapeHtml(T('wizard_pool.encryption_sub'))}</span></div>
        <tf-toggle id="nas-pw-encryption" ${state.encryption ? 'checked' : ''}></tf-toggle>
      </div>`;
  };

  const parityReason = (d) => {
    if (d.health === 'critical') return d.healthReason || T('wizard_pool.elastic_unavailable');
    if (!state.capabilities?.snapraid) return T('wizard_pool.elastic_no_snapraid');
    if (Number(d.sizeBytes) < Math.max(...picked().map((disk) => Number(disk.sizeBytes)))) return T('wizard_pool.elastic_small_parity');
    if (!state.parityIds.has(d.diskId) && (state.parityIds.size >= 2 || state.diskIds.size + state.parityIds.size >= 32)) return T('wizard_pool.elastic_parity_limit');
    return '';
  };
  const stepParity = () => `
    <h2 class="wizard-section-title">${escapeHtml(T('wizard_pool.elastic_parity'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(T('wizard_pool.elastic_parity_sub'))}</p>
    <div class="disk-cells" id="nas-pw-parity">${state.elasticDisks.filter((d) => !state.diskIds.has(d.diskId)).map((d) => {
      const reason = parityReason(d);
      return `<div class="disk-cell parity-pick ${state.parityIds.has(d.diskId) ? 'checked' : ''} ${reason ? 'disabled' : ''}" data-disk="${escapeAttr(d.diskId)}" title="${escapeAttr(reason)}">
        <tf-checkbox ${state.parityIds.has(d.diskId) ? 'checked' : ''} ${reason ? 'disabled' : ''}></tf-checkbox>
        <div class="dc-main"><div class="dc-name mono">${escapeHtml(d.name)}</div><div class="dc-sub">${escapeHtml(`${fmtBytes(d.sizeBytes)} · ${d.serial || '—'}`)}${reason ? ` · ${escapeHtml(reason)}` : ''}</div></div></div>`;
    }).join('')}</div>
    <div class="wizard-warning info mt-md">${sprite('info')}<div>${escapeHtml(T('wizard_pool.elastic_scope'))}</div></div>
    <tf-input class="mt-md" id="nas-pw-name" label="${escapeAttr(T('wizard_pool.name_label'))}" autocomplete="off" spellcheck="false" value="${escapeAttr(state.name)}"></tf-input>
    <tf-button class="mt-md" variant="secondary" data-pw-preview ${elasticDraftValid() && !state.planning ? '' : 'disabled'}>${escapeHtml(T('wizard_pool.elastic_preview'))}</tf-button>
    <div class="nas-elastic-preview" id="nas-pw-preview">${elasticPreviewHtml()}</div>`;

  const elasticDraftValid = () => elasticAvailable() && elasticNameValid(state.name) && picked().length === state.diskIds.size && state.diskIds.size > 0 && state.diskIds.size + state.parityIds.size <= 32 && state.parityIds.size <= 2 && state.capabilities.filesystems.includes(state.filesystem) && picked().every((d) => d.health !== 'critical') && parity().length === state.parityIds.size && parity().every((d) => !state.diskIds.has(d.diskId) && !parityReason(d));
  const elasticPreviewHtml = () => {
    if (state.planning) return `<p>${escapeHtml(I18n.t('common.loading'))}</p>`;
    if (state.planError) return `<div class="wizard-warning danger mt-md">${sprite('alert')}<div>${escapeHtml(state.planError)}</div></div>`;
    if (!state.plan) return '';
    return `${state.plan.refusals.map((r) => `<div class="wizard-warning danger mt-md">${sprite('alert')}<div>${escapeHtml(r.detail)}</div></div>`).join('')}
      ${state.plan.warnings.map((w) => `<div class="wizard-warning info mt-md">${sprite('info')}<div>${escapeHtml(w)}</div></div>`).join('')}
      ${state.plan.refusals.length ? '' : `<div class="explain-box mt-md">${escapeHtml(T('wizard_pool.sum_usable'))}: ${escapeHtml(fmtBytes(state.plan.usableBytes))} · ${escapeHtml(state.plan.unionPath)}</div>`}`;
  };

  const elasticSummary = () => `
    <h2 class="wizard-section-title">${escapeHtml(T('wizard_pool.summary_title'))}</h2>
    <tf-table id="nas-pw-summary" variant="flush" narrow><tf-column key="label" label="${escapeAttr(T('wizard_pool.summary_title'))}" renderer="html" width="40%"></tf-column><tf-column key="value" label="${escapeAttr(T('wizard_pool.selected'))}" renderer="html" width="60%" fill></tf-column></tf-table>
    <div class="explain-box mt-md">${escapeHtml(state.parityIds.size ? T('wizard_pool.elastic_protection') : T('wizard_pool.elastic_no_parity'))}</div>
    <ul class="loss-list mt-md">${erased().map((d) => `<li class="ll bad">${sprite('alert')}<span><span class="mono">${escapeHtml(d.name)}</span> · ${escapeHtml(d.model || '—')} · <span class="mono">${escapeHtml(d.serial || '—')}</span> — ${escapeHtml(T('wizard_pool.loss_erased'))}</span></li>`).join('')}</ul>
    <div class="confirm-type mt-md"><tf-input id="nas-pw-confirm" label="${escapeAttr(T('wizard_pool.retype'))}" autocomplete="off" spellcheck="false" placeholder="${escapeAttr(state.name)}" value="${escapeAttr(state.confirm)}" ${state.busy || state.submitted ? 'disabled' : ''}></tf-input></div>`;

  // Step 4 — summary with the loss list and the retype gate; after the job
  // starts the same step shows its progress and log, then the result.
  const stepSummary = () => {
    if (state.result) {
      const ok = state.result.ok;
      if (state.outcome?.outcome === 'approval' || state.outcome?.outcome === 'unknown') return `<div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(state.result.detail)}</div></div>`;
      return `<div class="result-box ${ok ? 'ok' : 'err'}">${sprite(ok ? 'check-circle' : 'alert')}<h3>${escapeHtml(ok ? T('wizard_pool.done_title', { name: state.name }) : T('wizard_pool.failed_title'))}</h3><p>${escapeHtml(state.result.detail || '')}</p></div>
        ${state.job ? `<pre class="job-log mono">${escapeHtml((state.job.log || []).join('\n'))}</pre>` : ''}`;
    }
    if (state.job) {
      return `
        <h2 class="wizard-section-title">${escapeHtml(T('wizard_pool.creating_title', { name: state.name }))}</h2>
        <p class="wizard-section-sub">${escapeHtml(T('wizard_pool.creating_sub'))}</p>
        <tf-progress-bar value="${Number(state.job.progressPct) || 0}" tone="accent" label="${escapeAttr(T('jobs.status_' + state.job.status))}"></tf-progress-bar>
        <pre class="job-log mono mt-sm">${escapeHtml((state.job.log || []).join('\n'))}</pre>`;
    }
    if (state.kind === 'elastic') return elasticSummary();
    const plan = state.plan;
    const chosen = plan && plan.options.find((o) => o.layout === state.layout);
    const disks = picked();
    const layoutValue = T('wizard_pool.sum_layout_value', { layout: layoutLabel(state.layout), n: disks.length, size: fmtBytes(plan?.smallestDiskBytes), disks: disks.map((d) => d.name).join(', ') });
    return `
      <h2 class="wizard-section-title">${escapeHtml(T('wizard_pool.summary_title'))}</h2>
      <div class="stat-rows">
        <div class="sr"><span class="k">${escapeHtml(T('wizard_pool.sum_pool'))}</span><span class="v mono fw-700">${escapeHtml(state.name)}</span></div>
        <div class="sr"><span class="k">${escapeHtml(T('wizard_pool.sum_layout'))}</span><span class="v">${escapeHtml(layoutValue)}</span></div>
        <div class="sr"><span class="k">${escapeHtml(T('wizard_pool.sum_usable'))}</span><span class="v mono fw-700">${escapeHtml(chosen ? fmtBytes(chosen.usableBytes) : '—')}</span></div>
        <div class="sr"><span class="k">${escapeHtml(T('wizard_pool.compression_label'))}</span><span class="v">${escapeHtml(state.compression)}</span></div>
      </div>
      <ul class="loss-list mt-md">${disks.map((d) => `<li class="ll bad">${sprite('alert')}<span><span class="mono">${escapeHtml(d.name)}</span> · ${escapeHtml(d.model || '—')} · <span class="mono">${escapeHtml(d.serial || '—')}</span> — ${escapeHtml(T('wizard_pool.loss_erased'))}</span></li>`).join('')}</ul>
      <div class="confirm-type mt-md">
        <div class="field">
          <label>${escapeHtml(T('wizard_pool.retype'))}</label>
          <tf-input id="nas-pw-confirm" autocomplete="off" spellcheck="false" placeholder="${escapeAttr(state.name)}" value="${escapeAttr(state.confirm)}"></tf-input>
        </div>
      </div>`;
  };

  const canProceed = () => {
    if (!active() || state.busy || state.submitted || !screen.isAdmin) return false;
    if (state.step === 0) return state.kind === 'zfs' || (state.kind === 'elastic' && elasticAvailable());
    if (state.step === 1) return state.diskIds.size > 0 && (state.kind !== 'elastic' || (state.diskIds.size <= 32 && state.capabilities.filesystems.includes(state.filesystem)));
    if (state.kind === 'elastic') return elasticDraftValid() && state.planRevision === state.revision && state.plan?.refusals.length === 0 && (state.step !== 3 || state.confirm === state.name);
    if (state.step === 2) return Boolean(state.plan) && Boolean(state.layout) && poolNameValid(state.name);
    if (state.step === 3) return !state.job && state.confirm.trim() === state.name;
    return true;
  };

  const footer = () => {
    const last = state.step === 3;
    const finished = last && state.result;
    const running = last && state.job && !state.result;
    const n = state.kind === 'elastic' ? state.plan?.wipedDevices.length || 0 : state.diskIds.size;
    let next;
    if (finished) next = `<tf-button variant="primary" icon="check" data-wizard-next>${escapeHtml(state.outcome?.outcome === 'approval' ? T('wizard_pool.elastic_jobs') : state.outcome?.outcome === 'unknown' ? T('wizard_pool.elastic_list') : state.outcome ? T('wizard_pool.elastic_details') : I18n.t('common.close'))}</tf-button>`;
    else if (last) next = `<tf-button variant="danger" icon="layers" data-wizard-next ${canProceed() && !running ? '' : 'disabled'}>${escapeHtml(T('wizard_pool.create_button', { n }))}</tf-button>`;
    else next = `<tf-button variant="primary" icon="chevron-right" data-wizard-next ${canProceed() ? '' : 'disabled'}>${escapeHtml(I18n.t('common.next'))}</tf-button>`;
    return `
      <tf-button variant="ghost" data-wizard-cancel ${running ? 'disabled' : ''}>${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="ghost" icon="chevron-left" data-wizard-back ${state.step === 0 || state.busy || state.submitted || running || finished ? 'disabled' : ''}>${escapeHtml(I18n.t('common.back'))}</tf-button>
      <span class="spacer"></span>
      ${next}`;
  };

  const syncNext = () => {
    const btn = win.querySelector('[data-wizard-next]');
    if (!btn || state.result) return;
    if (canProceed()) btn.removeAttribute('disabled');
    else btn.setAttribute('disabled', '');
  };

  const draw = () => {
    win.innerHTML = `
      <div slot="body">
        ${header()}
        <div class="install-step-body">${[stepKind, stepDisks, stepLayout, stepSummary][state.step]()}</div>
      </div>
      <div slot="footer">${footer()}</div>`;
    wire();
  };

  const wire = () => {
    win.querySelector('#nas-pw-kind')?.addEventListener('change', (e) => {
      if (!active() || !['zfs', 'elastic'].includes(e.detail.value) || (e.detail.value === 'elastic' && !elasticAvailable())) return;
      state.kind = e.detail.value; state.diskIds.clear(); state.parityIds.clear(); invalidatePlan(); draw();
    });
    win.querySelector('[data-pw-environment]')?.addEventListener('click', () => { if (active()) { win.close(); screen.switchTab('environment'); } });
    const fs = win.querySelector('#nas-pw-filesystem');
    if (fs) {
      fs.addEventListener('change', (e) => { state.filesystem = e.detail.value; invalidatePlan(); syncNext(); });
    }
    const parityCells = win.querySelector('#nas-pw-parity');
    if (parityCells) {
      parityCells.addEventListener('click', toggleCellCheckbox);
      parityCells.addEventListener('change', (e) => {
        const cell = e.target.closest('.disk-cell[data-disk]');
        const d = selectedMap().get(cell?.dataset.disk);
        if (!active() || !d || parityReason(d) || state.diskIds.has(d.diskId)) return;
        if (e.detail?.checked) state.parityIds.add(d.diskId); else state.parityIds.delete(d.diskId);
        invalidatePlan(); draw();
      });
    }
    win.querySelector('[data-pw-preview]')?.addEventListener('click', () => { if (elasticDraftValid() && !state.planning && active()) loadPlan(); });
    const summary = win.querySelector('#nas-pw-summary');
    if (summary) summary.rows = [
      { label: T('wizard_pool.sum_pool'), value: state.name },
      ...picked().map((d) => ({ label: T('wizard_pool.elastic_data'), value: `${d.name} · ${d.serial || '—'} · ${fmtBytes(d.sizeBytes)} · ${state.filesystem}` })),
      ...parity().map((d) => ({ label: T('wizard_pool.elastic_parity'), value: `${d.name} · ${d.serial || '—'} · ${fmtBytes(d.sizeBytes)} · SnapRAID` })),
      { label: T('wizard_pool.sum_usable'), value: fmtBytes(state.plan.usableBytes) },
      { label: T('wizard_pool.elastic_union'), value: state.plan.unionPath },
    ].map((row) => Object.fromEntries(Object.entries(row).map(([key, value]) => [key, `<span style="white-space:normal;overflow-wrap:anywhere">${escapeHtml(value)}</span>`])));
    const cells = win.querySelector('#nas-pw-disks');
    if (cells) {
      cells.addEventListener('click', toggleCellCheckbox);
      cells.addEventListener('change', (e) => {
        const cell = e.target.closest('.disk-cell[data-disk]');
        if (!active() || !cell || cell.classList.contains('disabled')) return;
        const cb = cell.querySelector('tf-checkbox');
        const on = typeof e.detail?.checked === 'boolean' ? e.detail.checked : Boolean(cb.checked);
        if (on) state.diskIds.add(cell.dataset.disk); else state.diskIds.delete(cell.dataset.disk);
        state.parityIds.delete(cell.dataset.disk);
        cell.classList.toggle('checked', on);
        // A different selection invalidates the plan the layout step cached.
        invalidatePlan();
        win.querySelector('#nas-pw-selected').textContent = selectedText();
        win.querySelector('#nas-pw-erase').textContent = eraseText();
        syncNext();
      });
    }
    win.querySelector('#nas-pw-layout')?.addEventListener('change', (e) => {
      state.layout = e.detail.value;
      const chosen = state.plan.options.find((o) => o.layout === state.layout);
      const box = win.querySelector('#nas-pw-explain');
      if (box) box.innerHTML = explainHtml(chosen);
      syncNext();
    });
    const name = win.querySelector('#nas-pw-name');
    if (name) {
      const onName = () => {
        state.name = name.value.trim();
        if (state.kind === 'elastic') invalidatePlan();
        if (state.name && !(state.kind === 'elastic' ? elasticNameValid(state.name) : poolNameValid(state.name))) name.setAttribute('error', state.kind === 'elastic' ? T('wizard_pool.elastic_name_invalid') : T('wizard_pool.name_invalid'));
        else name.removeAttribute('error');
        const preview = win.querySelector('[data-pw-preview]');
        if (preview) preview.toggleAttribute('disabled', !elasticDraftValid());
        const box = win.querySelector('#nas-pw-preview');
        if (box) box.innerHTML = '';
        syncNext();
      };
      name.addEventListener('input', onName);
      name.addEventListener('change', onName);
    }
    const comp = win.querySelector('#nas-pw-compression');
    if (comp) {
      comp.setOptions(COMPRESSION_OPTIONS.map((v) => ({ value: v, label: T('compression.' + v) })), state.compression);
      comp.addEventListener('change', (e) => { state.compression = e.detail.value; });
    }
    win.querySelector('#nas-pw-encryption')?.addEventListener('change', (e) => { state.encryption = Boolean(e.detail?.checked ?? e.target.checked); });
    const confirm = win.querySelector('#nas-pw-confirm');
    if (confirm) {
      const onConfirm = () => { state.confirm = confirm.value; syncNext(); };
      confirm.addEventListener('input', onConfirm);
      confirm.addEventListener('change', onConfirm);
      confirm.addEventListener('keydown', (e) => { if (e.key === 'Enter' && canProceed()) next(); });
    }
    win.querySelector('[data-wizard-cancel]')?.addEventListener('click', () => win.close());
    win.querySelector('[data-wizard-back]')?.addEventListener('click', () => { if (active() && state.step > 0 && !state.busy && !state.submitted && !state.job) { state.step--; if (state.kind === 'elastic') invalidatePlan(); draw(); } });
    win.querySelector('[data-wizard-next]')?.addEventListener('click', next);
  };

  const next = async () => {
    if (state.step === 3 && state.result) { win.close(); return; }
    if (!canProceed()) return;
    if (state.step === 1) {
      state.step = 2;
      draw();
      if (!state.plan && state.kind === 'zfs') await loadPlan();
      return;
    }
    if (state.step === 3) { await run(); return; }
    state.step++;
    draw();
  };

  const loadPlan = async () => {
    if (!active()) return;
    const revision = state.revision;
    const kind = state.kind;
    const snapshot = draft();
    const current = () => active() && state.revision === revision && state.kind === kind;
    state.planning = true;
    state.planError = '';
    if (kind === 'elastic') draw();
    try {
      const r = await screen.nas(kind === 'elastic' ? 'tentaNasElasticArrayPlanRequest' : 'tentaNasPoolPlanRequest', kind === 'elastic' ? snapshot : { diskIds: [...state.diskIds] });
      if (!current()) return;
      if (kind === 'elastic') {
        const p = r?.plan;
        if (!p || !Array.isArray(p.refusals) || !p.refusals.every((r) => r && typeof r.detail === 'string') || !Array.isArray(p.warnings) || !p.warnings.every((w) => typeof w === 'string') || !Array.isArray(p.wipedDevices) || (!p.refusals.length && (!Number.isFinite(p.usableBytes) || p.usableBytes <= 0 || p.unionPath !== `/mnt/${snapshot.name}` || p.wipedDevices.length !== erased().length || new Set(p.wipedDevices).size !== p.wipedDevices.length || !p.wipedDevices.every((p) => typeof p === 'string' && p.startsWith('/dev/')) || typeof p.stepsPreview !== 'string' || !p.stepsPreview))) throw new Error(T('wizard_pool.elastic_bad_plan'));
        state.plan = p;
        state.planRevision = revision;
      } else {
        state.plan = { options: r.options || [], warnings: r.warnings || [], smallestDiskBytes: Number(r.smallestDiskBytes) || 0 };
        const recommended = state.plan.options.find((o) => o.recommended && o.available) || state.plan.options.find((o) => o.available);
        state.layout = recommended ? recommended.layout : '';
      }
    } catch (e) {
      if (!current()) return;
      state.plan = null;
      state.planError = errMessage(e);
    }
    if (current()) { state.planning = false; if (state.step === 2) draw(); }
  };

  // ashift and autotrim are the node's call (the codec sends its defaults):
  // the mockup keeps the options step to name, compression and encryption.
  const run = async () => {
    if (!canProceed()) return;
    state.busy = true;
    const revision = state.revision;
    const kind = state.kind;
    const current = () => active() && state.revision === revision && state.kind === kind && (kind !== 'elastic' || state.confirm === payload.confirmName);
    const payload = kind === 'elastic' ? { name: state.name, filesystem: state.filesystem, dataDiskIds: [...state.diskIds], parityDiskIds: [...state.parityIds], confirmName: state.confirm } : {
      name: state.name,
      layout: state.layout,
      diskIds: [...state.diskIds],
      compression: state.compression,
      encryption: state.encryption,
    };
    draw();
    let sent = false;
    let res;
    try {
      res = await screen.withSudo((sudoPassword) => {
        if (!current() || sent) return null;
        sent = true;
        state.submitted = kind === 'elastic';
        if (kind === 'elastic') state.outcome = { outcome: 'unknown' };
        return screen.nas(kind === 'elastic' ? 'tentaNasElasticArrayCreateRequest' : 'tentaNasPoolCreateRequest', { ...payload, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS });
      }, T('wizard_pool.sudo_title', { name: state.name }), current);
    } catch (e) {
      if (kind !== 'elastic') toast(errMessage(e), 'error');
    }
    if (!current()) return;
    state.busy = false;
    if (kind === 'elastic' && sent) {
      if (res?.approval?.requestId) {
        state.outcome = { outcome: 'approval', approvalId: res.approval.requestId };
        state.result = { ok: false, detail: T('wizard_pool.elastic_approval') };
      } else if (!res?.job?.jobId) {
        state.outcome = { outcome: 'unknown' };
        state.result = { ok: false, detail: T('wizard_pool.elastic_unknown') };
      } else state.outcome = { outcome: 'job', jobId: res.job.jobId };
    }
    if (!res?.job?.jobId) { draw(); return; }
    state.job = res.job;
    toast(T('jobs.started', { kind: jobKindLabel(res.job.kind) }), 'success');
    draw();
    await pollJob();
  };

  const pollJob = async () => {
    if (!active() || !state.job) return;
    const jobId = state.job.jobId;
    try {
      const r = await screen.nas('tentaNasJobGetRequest', { jobId });
      if (!active()) return;
      if (!r?.job || r.job.jobId !== jobId) throw new Error(T('wizard_pool.elastic_unknown'));
      state.job = r.job;
    } catch (e) {
      if (!active()) return;
      state.result = { ok: false, detail: state.kind === 'elastic' ? T('wizard_pool.elastic_unknown') : errMessage(e) };
      if (state.kind === 'elastic') state.outcome = { outcome: 'unknown' };
      draw();
      return;
    }
    const s = state.job.status;
    if (s === 'running' || s === 'queued') {
      draw();
      state.timer = setTimeout(pollJob, POLL_JOB_MODAL_MS);
      return;
    }
    const ok = s === 'succeeded' || s === 'done';
    state.result = { ok, detail: ok ? T('wizard_pool.done_detail', { name: state.name }) : (state.job.error || T('jobs.status_' + s)) };
    draw();
    if (onDone && state.kind === 'zfs') onDone(state.job);
  };

  win.addEventListener('close-request', () => {
    state.closed = true;
    if (state.timer) clearTimeout(state.timer);
    if (screen.openWindow === win) screen.openWindow = null;
    notifyCreated();
  });
  document.body.appendChild(win);
  draw();
  (async () => {
    try {
      const r = await screen.nas('tentaNasElasticCapabilitiesRequest');
      if (!active()) return;
      if (!r?.capabilities || !Array.isArray(r.capabilities.filesystems) || !Array.isArray(r.freeDisks)) throw new Error(T('wizard_pool.elastic_unavailable'));
      state.capabilities = r.capabilities;
      state.elasticDisks = r.freeDisks;
      state.filesystem = r.capabilities.filesystems.includes('xfs') ? 'xfs' : r.capabilities.filesystems.includes('ext4') ? 'ext4' : '';
    } catch (e) {
      if (!active()) return;
      state.capabilitiesError = errMessage(e);
    }
    if (active() && state.step === 0) draw();
  })();
  return win;
}

// A click anywhere on a disk cell toggles its checkbox; the checkbox handles
// its own clicks, so only clicks outside it are forwarded.
export function toggleCellCheckbox(e) {
  const cell = e.target.closest('.disk-cell[data-disk]');
  if (!cell || cell.classList.contains('disabled') || e.target.closest('tf-checkbox')) return;
  const cb = cell.querySelector('tf-checkbox');
  if (!cb || cb.hasAttribute('disabled')) return;
  cb.checked = !cb.checked;
  cb.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { checked: cb.checked } }));
}

function reasonLabel(reason) {
  const key = 'wizard_pool.reason_' + (reason || 'unsupported');
  const label = T(key);
  return label === 'tentanas.' + key ? String(reason) : label;
}
