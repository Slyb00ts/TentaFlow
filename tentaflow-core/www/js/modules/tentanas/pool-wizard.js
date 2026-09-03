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

// zpool(8) naming: a pool name starts with a letter and must not be one of
// the vdev keywords; the node re-checks, this only keeps the button honest.
const NAME_RE = /^[a-zA-Z][a-zA-Z0-9_.:-]*$/;
const RESERVED = new Set(['mirror', 'raidz', 'raidz1', 'raidz2', 'raidz3', 'draid', 'spare', 'log', 'cache', 'special', 'dedup']);
export const poolNameValid = (name) => NAME_RE.test(name) && !RESERVED.has(name.toLowerCase());

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
export function openPoolWizard(screen, { freeDisks = [], pools = [], onDone = null } = {}) {
  if (screen.openWindow) { screen.openWindow.remove(); screen.openWindow = null; }
  const node = screen.currentNode();
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
  };
  const steps = [T('wizard_pool.step_kind'), T('wizard_pool.step_disks'), T('wizard_pool.step_layout'), T('wizard_pool.step_summary')];
  const subs = [T('wizard_pool.sub_kind'), T('wizard_pool.sub_disks'), T('wizard_pool.header_sub_layout'), T('wizard_pool.sub_summary')];
  const allDisks = wizardDisks(freeDisks, pools);
  const diskById = new Map(freeDisks.map((d) => [d.diskId, d]));
  const picked = () => [...state.diskIds].map((id) => diskById.get(id)).filter(Boolean);
  // Capacity of a vdev follows its smallest member, so "2 × 8 TB" quotes
  // the smallest picked disk, exactly as the node's plan will.
  const smallestBytes = () => picked().reduce((a, d) => Math.min(a, Number(d.sizeBytes) || 0), Infinity);
  const selectedText = () => (state.diskIds.size ? T('wizard_pool.selected_value', { n: state.diskIds.size, size: fmtBytes(smallestBytes()) }) : '0');
  const eraseText = () => {
    const serials = picked().map((d) => d.serial || d.name);
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

  const header = () => {
    win.setAttribute('title', state.step > 0 ? T('wizard_pool.heading_kind', { kind: KIND_LABELS[state.kind] }) : T('wizard_pool.title'));
    return `
    <div class="install-header">
      <div class="big-ico">${sprite('layers')}</div>
      <div class="install-header-meta">
        <h1>${escapeHtml(T('wizard_pool.heading'))} <span class="version">${escapeHtml(T('wizard.node_tag', { node: node.nodeName }))}</span></h1>
        <div class="sub">${escapeHtml(subs[state.step])}</div>
      </div>
    </div>
    <div class="install-progress">${steps.map((s, i) => `<div class="install-step ${i === state.step ? 'active' : i < state.step ? 'done' : ''}"><span class="num">${i < state.step ? sprite('check') : i + 1}</span><span class="label">${escapeHtml(s)}</span></div>`).join('')}</div>`;
  };

  // Step 1 — pool type. Only ZFS is buildable in this phase; the other two
  // tiles stay visible so the admin sees where the product is going, and
  // the AnyRAID tooltip says which OpenZFS the node runs.
  const stepKind = () => `
    <h2 class="wizard-section-title">${escapeHtml(T('wizard_pool.kind_title'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(T('wizard_pool.kind_sub'))}</p>
    <tf-choice-group id="nas-pw-kind" value="${escapeAttr(state.kind)}" columns="3">
      <tf-choice-card value="zfs" icon="layers" heading="ZFS" description="${escapeAttr(T('wizard_pool.kind_zfs_desc'))}"></tf-choice-card>
      <tf-choice-card value="anyraid" icon="layers" heading="ZFS AnyRAID" description="${escapeAttr(T('wizard_pool.kind_anyraid_desc'))}" title="${escapeAttr(T('wizard_pool.kind_anyraid_title', { v: zfsVersion }))}" disabled></tf-choice-card>
      <tf-choice-card value="elastic" icon="cylinder" heading="Elastic Array" description="${escapeAttr(T('wizard_pool.kind_elastic_desc'))}" disabled></tf-choice-card>
    </tf-choice-group>
    <div class="text-xs text-3 mt-md">${escapeHtml(T('wizard_pool.kind_hint'))}</div>`;

  // Step 2 — disks. Members and spares of other pools are disabled with the
  // reason; a free disk with a critical SMART verdict cannot be picked either.
  const stepDisks = () => {
    const cells = allDisks.map(({ disk: d, reason }) => {
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
      <h2 class="wizard-section-title">${escapeHtml(T('wizard_pool.disks_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('wizard_pool.disks_sub'))}</p>
      ${allDisks.length ? `<div class="disk-cells" id="nas-pw-disks">${cells}</div>` : `<div class="muted">${escapeHtml(T('pools.no_free_disks'))}</div>`}
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

  // Step 4 — summary with the loss list and the retype gate; after the job
  // starts the same step shows its progress and log, then the result.
  const stepSummary = () => {
    if (state.result) {
      const ok = state.result.ok;
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
    if (state.step === 0) return state.kind === 'zfs';
    if (state.step === 1) return state.diskIds.size > 0;
    if (state.step === 2) return Boolean(state.plan) && Boolean(state.layout) && poolNameValid(state.name);
    if (state.step === 3) return !state.job && state.confirm.trim() === state.name;
    return true;
  };

  const footer = () => {
    const last = state.step === 3;
    const finished = last && state.result;
    const running = last && state.job && !state.result;
    const n = state.diskIds.size;
    let next;
    if (finished) next = `<tf-button variant="primary" icon="check" data-wizard-next>${escapeHtml(I18n.t('common.close'))}</tf-button>`;
    else if (last) next = `<tf-button variant="danger" icon="layers" data-wizard-next ${canProceed() && !running ? '' : 'disabled'}>${escapeHtml(T('wizard_pool.create_button', { n }))}</tf-button>`;
    else next = `<tf-button variant="primary" icon="chevron-right" data-wizard-next ${canProceed() ? '' : 'disabled'}>${escapeHtml(I18n.t('common.next'))}</tf-button>`;
    return `
      <tf-button variant="ghost" data-wizard-cancel ${running ? 'disabled' : ''}>${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="ghost" icon="chevron-left" data-wizard-back ${state.step === 0 || running || finished ? 'disabled' : ''}>${escapeHtml(I18n.t('common.back'))}</tf-button>
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
    win.querySelector('#nas-pw-kind')?.addEventListener('change', (e) => { state.kind = e.detail.value; syncNext(); });
    const cells = win.querySelector('#nas-pw-disks');
    if (cells) {
      cells.addEventListener('click', toggleCellCheckbox);
      cells.addEventListener('change', (e) => {
        const cell = e.target.closest('.disk-cell[data-disk]');
        if (!cell || cell.classList.contains('disabled')) return;
        const cb = cell.querySelector('tf-checkbox');
        const on = typeof e.detail?.checked === 'boolean' ? e.detail.checked : Boolean(cb.checked);
        if (on) state.diskIds.add(cell.dataset.disk); else state.diskIds.delete(cell.dataset.disk);
        cell.classList.toggle('checked', on);
        // A different selection invalidates the plan the layout step cached.
        state.plan = null;
        state.layout = '';
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
        if (state.name && !poolNameValid(state.name)) name.setAttribute('error', T('wizard_pool.name_invalid'));
        else name.removeAttribute('error');
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
    win.querySelector('[data-wizard-back]')?.addEventListener('click', () => { if (state.step > 0 && !state.job) { state.step--; draw(); } });
    win.querySelector('[data-wizard-next]')?.addEventListener('click', next);
  };

  const next = async () => {
    if (state.step === 3 && state.result) { win.close(); return; }
    if (!canProceed()) return;
    if (state.step === 1) {
      state.step = 2;
      draw();
      if (!state.plan) await loadPlan();
      return;
    }
    if (state.step === 3) { await run(); return; }
    state.step++;
    draw();
  };

  const loadPlan = async () => {
    state.planError = '';
    try {
      const r = await screen.nas('tentaNasPoolPlanRequest', { diskIds: [...state.diskIds] });
      state.plan = { options: r.options || [], warnings: r.warnings || [], smallestDiskBytes: Number(r.smallestDiskBytes) || 0 };
      const recommended = state.plan.options.find((o) => o.recommended && o.available) || state.plan.options.find((o) => o.available);
      state.layout = recommended ? recommended.layout : '';
    } catch (e) {
      state.plan = null;
      state.planError = errMessage(e);
    }
    if (state.step === 2 && win.isConnected) draw();
  };

  // ashift and autotrim are the node's call (the codec sends its defaults):
  // the mockup keeps the options step to name, compression and encryption.
  const run = async () => {
    const payload = {
      name: state.name,
      layout: state.layout,
      diskIds: [...state.diskIds],
      compression: state.compression,
      encryption: state.encryption,
    };
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolCreateRequest', { ...payload, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('wizard_pool.sudo_title', { name: state.name }));
    if (!res || !res.job) return;
    state.job = res.job;
    toast(T('jobs.started', { kind: jobKindLabel(res.job.kind) }), 'success');
    draw();
    await pollJob();
  };

  const pollJob = async () => {
    if (!win.isConnected || !state.job) return;
    try {
      const r = await screen.nas('tentaNasJobGetRequest', { jobId: state.job.jobId });
      state.job = r.job;
    } catch (e) {
      state.result = { ok: false, detail: errMessage(e) };
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
    if (onDone) onDone(state.job);
  };

  win.addEventListener('close-request', () => {
    if (state.timer) clearTimeout(state.timer);
    if (screen.openWindow === win) screen.openWindow = null;
  });
  draw();
  document.body.appendChild(win);
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
