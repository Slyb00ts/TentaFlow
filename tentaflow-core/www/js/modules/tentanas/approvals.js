// ===== File: modules/tentanas/approvals.js — the "Oczekujące na zatwierdzenie" list of the Tasks tab (n15) and the dialogs of the four-eyes flow (plan-02 §5.10) =====
//
// A red-path request answers with `{ approval }` instead of `{ job }` when the
// node parked it: nothing ran, and the row below is what to watch. The list
// shows every open request; the approve button is disabled on the caller's own
// request, but that is a courtesy — the node refuses the author regardless, so
// this module never has to be the last line of defence.
//
// The fleet switch lives in the card's head because it is the same decision the
// list is about: with it off, only the snapshot release still parks (it has no
// other way to happen at all).

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, fmtAgo, fmtIn, fmtDate, fmtDuration, fmtSchedule, errMessage, ADMIN_TIMEOUT_MS, jobAuthor, wordReasons, nodeTextTitle } from '/js/modules/tentanas/format.js';
import { setAttr, setText, patchHtml } from '/js/lib/dom-patch.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-input.js';
import '/js/components/tf-window.js';

// Every REACHABLE `OP_*` constant of `tentanas/approvals.rs`. A parked
// operation missing from this list degrades to the generic "Operacja" label,
// which tells the approving admin nothing about what they are approving — and
// `elastic_fix` (a repair that overwrites blocks), `elastic_add_disk` (a disk
// gets formatted) and `elastic_destroy` (an array stops serving) were all
// missing, which is exactly the set where the label matters most.
//
// `elastic_replace_disk` is deliberately NOT here: disk replacement is
// withdrawn, the node refuses the request before anything is parked, so a
// label for it would describe an operation that cannot appear.
const OPERATIONS = ['pool_destroy', 'snapshot_release', 'share_delete', 'target_delete', 'config_import', 'elastic_create', 'elastic_restore', 'elastic_sync', 'elastic_scrub', 'elastic_mover', 'elastic_fix', 'elastic_add_disk', 'elastic_add_disk_abort', 'elastic_destroy', 'elastic_schedule'];

export const operationLabel = (op) => T('approvals.op_' + (OPERATIONS.includes(op) ? op : 'unknown'));

// A parked operation's detail in the approver's language (wave 6): the node
// parks every request with its detail as a code and parameters
// (`detailReasons`, `approvals::park`) beside its own English sentence, which
// becomes the tooltip. Above all the data-loss warning of a Sync over a parity
// fault has to reach a de/en/fr/es approver in words they read.
//
// An older node wrote only the sync's code, as `text:<code>` in `detail`;
// every other older detail is its sentence, shown as written. A code this
// build has no words for shows the sentence too, rather than nothing.
const CODED_DETAIL = /^text:([a-z0-9_]+)$/;
const SCHEDULE_TASKS = new Set(['mover', 'sync', 'scrub']);
const yesNo = (v) => (v === 'true' ? T('approvals.detail.yes') : v === 'false' ? T('approvals.detail.no') : null);
const DETAIL_WORDS = new Map([
  ['pool_destroy', (p) => (p.pool ? T('approvals.detail.pool_destroy', { pool: p.pool }) : null)],
  ['share_delete', (p) => (p.share && p.path ? T('approvals.detail.share_delete', { share: p.share, path: p.path }) : null)],
  ['target_delete', (p) => (p.target ? T('approvals.detail.target_delete', { target: p.target, sources: p.sources || '—' }) : null)],
  ['snapshot_release', (p) => {
    if (!p.snapshot) return null;
    // The author's reason is their own words, carried as written.
    return p.reason
      ? T('approvals.detail.snapshot_release_reason', { snapshot: p.snapshot, reason: p.reason })
      : T('approvals.detail.snapshot_release', { snapshot: p.snapshot });
  }],
  ['config_import', (p) => (p.count && p.items ? T('approvals.detail.config_import', { count: p.count, items: p.items }) : null)],
  ['elastic_create', (p) => (p.array ? T('approvals.detail.elastic_create', { array: p.array }) : null)],
  ['elastic_sync', () => T('approvals.detail_elastic_sync')],
  ['elastic_sync_over_fault', () => T('approvals.detail_elastic_sync_over_fault')],
  ['elastic_scrub', () => T('approvals.detail.elastic_scrub')],
  ['elastic_fix', (p) => {
    // The disk by its kernel name, else by its number — never by the slot
    // the request keys it by.
    if (p.disk) return T('approvals.detail.elastic_fix', { disk: p.disk });
    return /^\d+$/.test(String(p.number || '')) ? T('approvals.detail.elastic_fix_number', { n: p.number }) : T('approvals.detail.elastic_fix_unnamed');
  }],
  ['elastic_add_disk', (p) => (p.disk ? T('approvals.detail.elastic_add_disk', { disk: p.disk }) : T('approvals.detail.elastic_add_disk_unnamed'))],
  ['elastic_add_disk_abort', () => T('approvals.detail.elastic_add_disk_abort')],
  ['elastic_destroy', () => T('approvals.detail.elastic_destroy')],
  ['elastic_mover', (p) => (yesNo(p.coupled_sync) ? T(p.coupled_sync === 'true' ? 'approvals.detail.elastic_mover_coupled' : 'approvals.detail.elastic_mover_uncoupled') : null)],
  ['elastic_schedule', (p) => {
    if (!SCHEDULE_TASKS.has(p.task) || !['true', 'false'].includes(p.enabled) || !p.every) return null;
    const cadence = fmtSchedule({ every: p.every, hour: Number(p.hour) || 0, minute: Number(p.minute) || 0, weekday: Number(p.weekday) || 0, day: Number(p.day) || 1 });
    const head = T(`approvals.detail.elastic_schedule_${p.enabled === 'true' ? 'arm' : 'keep_off'}`, {
      task: T('approvals.detail.task_' + p.task),
      cadence,
    });
    if (p.min_age_secs == null && p.cache_min_free_pct == null) return head;
    const coupled = yesNo(p.coupled_sync);
    if (!coupled || !/^\d+$/.test(String(p.min_age_secs)) || !/^\d+$/.test(String(p.cache_min_free_pct))) return null;
    return T('approvals.detail.elastic_schedule_rules', {
      head,
      age: fmtDuration(Number(p.min_age_secs)),
      pct: p.cache_min_free_pct,
      coupled,
    });
  }],
]);

// `{ text, title }` of one approval's detail: the words, and the node's own
// sentence as the tooltip when the words replace it. Accepts the approval,
// or (an older caller) its bare `detail`.
export function approvalDetail(approval) {
  const a = typeof approval === 'string' ? { detail: approval } : approval || {};
  const detail = String(a.detail || '');
  const worded = wordReasons(a.detailReasons, DETAIL_WORDS);
  if (worded) return { text: worded, title: CODED_DETAIL.test(detail) ? '' : nodeTextTitle(detail) };
  const code = CODED_DETAIL.exec(detail)?.[1];
  if (code) {
    const legacy = wordReasons([{ code }], DETAIL_WORDS);
    if (legacy) return { text: legacy, title: '' };
  }
  return { text: detail, title: '' };
}

const STATUS_TONE = {
  pending: 'warn',
  approved: 'ok',
  rejected: 'info',
  expired: 'info',
  failed: 'err',
};

const statusTone = (status) => STATUS_TONE[status] || 'info';

/** The card the Tasks tab drops into its stack. `admin` gates every control. */
export function approvalsCardHtml(admin) {
  return `
    <div class="section-card" id="nas-approvals-card">
      <div class="section-card-head">
        <div class="title">${sprite('shield')} ${escapeHtml(T('approvals.title'))} <tf-chip size="sm" status="warn" id="nas-approvals-count" label="0"></tf-chip></div>
        <div class="actions">
          <span class="hint" id="nas-approvals-hint">${escapeHtml(T('approvals.hint'))}</span>
          ${admin ? `
          <tf-input id="nas-approvals-ttl" type="number" min="1" max="720" step="1" inputmode="numeric" label="${escapeAttr(T('approvals.ttl_label'))}"></tf-input>
          <tf-toggle id="nas-approvals-enabled" title="${escapeAttr(T('approvals.settings_title'))}"></tf-toggle>` : ''}
        </div>
      </div>
      <div class="muted" id="nas-approvals-settings"></div>
      <tf-table id="nas-approvals-table" actions-label="${escapeAttr(I18n.t('common.actions'))}" empty-message="${escapeAttr(T('approvals.none'))}">
        <tf-column key="operation" label="${escapeAttr(T('approvals.col_operation'))}" renderer="html" fill></tf-column>
        <tf-column key="subject" label="${escapeAttr(T('approvals.col_subject'))}" renderer="html" nowrap hide-below="900"></tf-column>
        <tf-column key="requested" label="${escapeAttr(T('approvals.col_requested'))}" renderer="html" nowrap></tf-column>
        <tf-column key="expires" label="${escapeAttr(T('approvals.col_expires'))}" renderer="html" nowrap hide-below="1024"></tf-column>
        <tf-column key="status" label="${escapeAttr(T('approvals.col_status'))}" renderer="html" nowrap></tf-column>
      </tf-table>
    </div>`;
}

/**
 * Wires the card written by `approvalsCardHtml`. Returns `{ refresh }`; the
 * caller polls it next to its other lists. `onExecuted` runs after an approval
 * really started something, so the tab can reload the jobs it created.
 */
export function wireApprovals(screen, body, { onExecuted = null } = {}) {
  const card = body.querySelector('#nas-approvals-card');
  const table = body.querySelector('#nas-approvals-table');
  const state = { approvals: [], settings: null };

  const paint = () => {
    const open = state.approvals.filter((a) => a.status === 'pending');
    // `setAttribute` with an identical value still re-renders the chip.
    setAttr(body.querySelector('#nas-approvals-count'), 'label', String(open.length));
    // The card stays out of the way while nothing waits and the switch is off:
    // an empty list plus a disabled feature is noise, not information.
    card.hidden = !open.length && !state.settings?.enabled;
    table.rows = state.approvals.map((a) => {
      const requester = jobAuthor(a.requestedBy);
      const decider = a.decidedBy ? jobAuthor(a.decidedBy) : null;
      return {
        _approval: a,
        operation: (() => {
          const detail = approvalDetail(a);
          return `<span class="tf-table__cell-title">${escapeHtml(operationLabel(a.operation))}</span><div class="tf-table__cell-sub"${detail.title ? ` title="${escapeAttr(detail.title)}"` : ''}>${escapeHtml(detail.text)}</div>`;
        })(),
        subject: a.subject ? `<span class="tf-table__cell--mono">${escapeHtml(a.subject)}</span>` : '—',
        requested: `<span>${escapeHtml(fmtAgo(a.requestedAt))}</span><div class="tf-table__cell-sub">${escapeHtml(T('approvals.requested_by', { user: requester.label }))}</div>`,
        expires: `<span class="tf-table__cell--mono">${escapeHtml(a.status === 'pending' ? fmtIn(a.expiresAt) : fmtDate(a.expiresAt))}</span>`,
        status: `<tf-chip size="sm" dot status="${statusTone(a.status)}" label="${escapeAttr(T('approvals.status_' + a.status))}"></tf-chip>${
          decider ? `<div class="tf-table__cell-sub">${escapeHtml(T('approvals.decided_by', { user: decider.label }))}</div>` : ''}`,
      };
    });
  };

  table.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const a = row._approval;
    const wrap = document.createElement('div');
    wrap.className = 'row-actions';
    if (a.status !== 'pending' || !screen.isAdmin) return wrap;
    if (a.isOwnRequest) {
      const note = document.createElement('span');
      note.className = 'muted';
      note.textContent = T('approvals.own_request');
      wrap.appendChild(note);
      return wrap;
    }
    for (const [act, icon, variant, label] of [
      ['approve', 'check', 'primary', T('approvals.approve')],
      ['reject', 'x', 'ghost', T('approvals.reject')],
    ]) {
      const b = document.createElement('tf-button');
      b.setAttribute('size', 'sm');
      b.setAttribute('variant', variant);
      b.setAttribute('icon', icon);
      b.textContent = label;
      b.addEventListener('click', (e) => { e.stopPropagation(); decide(live()._approval, act === 'approve'); });
      wrap.appendChild(b);
    }
    return wrap;
  };

  const paintSettings = () => {
    const s = state.settings;
    const el = body.querySelector('#nas-approvals-settings');
    // `setText` rather than `textContent =`: it also drops the patch cache, so
    // the same markup coming back later is not mistaken for "already there".
    if (!s) { setText(el, ''); return; }
    const toggle = body.querySelector('#nas-approvals-enabled');
    // Assigning `checked` writes the attribute unconditionally, and tf-toggle
    // re-renders in its attributeChangedCallback — so guard it.
    if (toggle) setAttr(toggle, 'checked', Boolean(s.enabled));
    const ttl = body.querySelector('#nas-approvals-ttl');
    // Only while the admin is not mid-edit: a poll must not overwrite what is
    // being typed — and only when the number actually moved.
    if (ttl && document.activeElement !== ttl && String(ttl.value) !== String(s.ttlHours)) ttl.value = String(s.ttlHours);
    const origin = s.byDefault
      ? T(s.enabled ? 'approvals.settings_default_on' : 'approvals.settings_default_off', { n: s.adminCount })
      : T('approvals.settings_admins', { n: s.adminCount });
    // This line is the same sentence on almost every poll — the settings
    // change when an admin changes them, not every 30 s. Rewriting it
    // destroyed and recreated it for nothing, which is exactly the flicker
    // the mockups forbid ("nigdy pełne odświeżenie całości").
    patchHtml(el, `${escapeHtml(T('approvals.settings_sub'))} <span class="text-3">${escapeHtml(origin)}</span>${
      s.adminCount < 2 ? `<div class="text-3">${escapeHtml(T('approvals.single_admin'))}</div>` : ''}`);
  };

  const apply = (res) => {
    state.approvals = res.approvals || [];
    state.settings = res.settings || null;
    paint();
    paintSettings();
  };

  const refresh = async () => {
    if (screen.disposed || !body.isConnected) return;
    try {
      const res = await screen.nas('tentaNasApprovalsListRequest', { includeClosed: false });
      if (screen.disposed || !body.isConnected) return;
      apply(res);
    } catch (e) {
      if (screen.disposed || !body.isConnected) return;
      toast(errMessage(e), 'error');
    }
  };

  const decide = async (approval, approve) => {
    const nodeId = screen.currentNode()?.nodeId;
    const surface = body.querySelector('#nas-approvals-table');
    const isCurrent = () => !screen.disposed && body.isConnected && surface?.isConnected && screen.currentNode()?.nodeId === nodeId;
    if (!isCurrent()) return;
    // A config import the fleet has no node name for arrives with no subject
    // (dispatch `config_import_subject`): the operation alone, never an id.
    const detail = approval.subject ? `${operationLabel(approval.operation)} — ${approval.subject}` : operationLabel(approval.operation);
    const note = await askDecision(approve, detail);
    if (note === null || !isCurrent()) return;
    // Approving RUNS the operation, so it needs the approver's own sudo
    // password in mode B; rejecting touches nothing on the node.
    const send = (sudoPassword) => isCurrent() ? screen.nas(
      'tentaNasApprovalDecideRequest',
      { requestId: approval.requestId, approve, note, sudoPassword },
      { timeoutMs: ADMIN_TIMEOUT_MS },
    ) : null;
    let res;
    try {
      res = approve
        ? await screen.withSudo(send, T('approvals.approve_title'), isCurrent)
        : await send(undefined);
    } catch (e) {
      if (!isCurrent()) return;
      toast(errMessage(e), 'error');
      refresh();
      return;
    }
    if (res === null || !isCurrent()) return;
    apply(res);
    toast(approve ? T('approvals.approved_done') : T('approvals.rejected_done'), 'success');
    if (approve && onExecuted) onExecuted();
  };

  // Both controls save the whole setting: the switch sends the TTL as it
  // stands, the TTL field sends the switch as it stands.
  const saveSettings = async (payload) => {
    try {
      apply(await screen.nas('tentaNasApprovalSettingsSetRequest', payload));
      toast(T('approvals.settings_saved'), 'success');
    } catch (err) {
      toast(errMessage(err), 'error');
      refresh();
    }
  };
  body.querySelector('#nas-approvals-enabled')?.addEventListener('change', (e) => {
    saveSettings({ enabled: Boolean(e.target.checked), ttlHours: Number(state.settings?.ttlHours) || 0 });
  });
  body.querySelector('#nas-approvals-ttl')?.addEventListener('change', (e) => {
    const hours = Math.max(1, Math.round(Number(e.target.value) || 0));
    saveSettings({ enabled: Boolean(state.settings?.enabled), ttlHours: hours });
  });

  paint();
  return { refresh };
}

/**
 * The decision dialog: what will happen, and a reason that goes to the audit
 * row. Resolves to the note (possibly empty), or `null` when cancelled.
 */
export function askDecision(approve, detail) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', approve ? T('approvals.approve_title') : T('approvals.reject_title'));
  win.setAttribute('icon', approve ? 'check' : 'x');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '520');
  win.setAttribute('min-width', '420');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="explain-box">${escapeHtml(approve ? T('approvals.approve_confirm', { detail }) : T('approvals.reject_confirm', { detail }))}</div>
      <tf-input id="nas-approval-note" label="${escapeAttr(T('approvals.note_label'))}" autocomplete="off" spellcheck="false"></tf-input>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="${approve ? 'danger' : 'primary'}" icon="${approve ? 'check' : 'x'}" data-action="confirm">${escapeHtml(approve ? T('approvals.approve') : T('approvals.reject'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  return new Promise((resolve) => {
    let settled = false;
    const done = (value) => { if (!settled) { settled = true; resolve(value); } };
    win.addEventListener('action', (e) => {
      if (e.detail?.action === 'confirm') {
        done(String(win.querySelector('#nas-approval-note').value || '').trim());
      } else {
        done(null);
      }
      win.close(true);
    });
    win.addEventListener('close', () => done(null));
  });
}

/**
 * What a parked answer looks like to the admin who asked. Called from
 * `followResponse`, so every red path reports the same thing rather than each
 * dialog inventing its own wording.
 */
export function reportParked(approval) {
  toast(T('approvals.parked'), 'warning');
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('approvals.parked'));
  win.setAttribute('icon', 'shield');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '520');
  win.setAttribute('min-width', '420');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="explain-box">${escapeHtml(T('approvals.parked_detail', { t: fmtIn(approval.expiresAt) }))}</div>
      <div class="stat-rows">
        <div class="sr"><span class="k">${escapeHtml(T('approvals.col_operation'))}</span><span class="v">${escapeHtml(operationLabel(approval.operation))}</span></div>
        <div class="sr"><span class="k">${escapeHtml(T('approvals.col_subject'))}</span><span class="v">${escapeHtml(approval.subject || '—')}</span></div>
      </div>
    </div>
    <div slot="footer">
      <tf-button variant="primary" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  win.addEventListener('action', () => win.close(true));
  return win;
}
