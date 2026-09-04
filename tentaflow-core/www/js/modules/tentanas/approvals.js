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
import { T, sprite, fmtAgo, fmtIn, fmtDate, errMessage, ADMIN_TIMEOUT_MS } from '/js/modules/tentanas/format.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-input.js';
import '/js/components/tf-window.js';

// Every `OP_*` constant of `tentanas/approvals.rs`. A parked operation missing
// from this list degrades to the generic "Operacja" label, which tells the
// approving admin nothing about what they are approving.
const OPERATIONS = ['pool_destroy', 'snapshot_release', 'share_delete', 'target_delete', 'config_import'];

export const operationLabel = (op) => T('approvals.op_' + (OPERATIONS.includes(op) ? op : 'unknown'));

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
        <tf-column key="expires" label="${escapeAttr(T('approvals.col_expires'))}" renderer="html" nowrap hide-below="1000"></tf-column>
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
    body.querySelector('#nas-approvals-count').setAttribute('label', String(open.length));
    // The card stays out of the way while nothing waits and the switch is off:
    // an empty list plus a disabled feature is noise, not information.
    card.hidden = !open.length && !state.settings?.enabled;
    table.rows = state.approvals.map((a) => ({
      _approval: a,
      operation: `<span class="tf-table__cell-title">${escapeHtml(operationLabel(a.operation))}</span><div class="tf-table__cell-sub">${escapeHtml(a.detail)}</div>`,
      subject: `<span class="tf-table__cell--mono">${escapeHtml(a.subject)}</span>`,
      requested: `<span>${escapeHtml(fmtAgo(a.requestedAt))}</span><div class="tf-table__cell-sub">${escapeHtml(T('approvals.requested_by', { user: a.requestedBy }))}</div>`,
      expires: `<span class="tf-table__cell--mono">${escapeHtml(a.status === 'pending' ? fmtIn(a.expiresAt) : fmtDate(a.expiresAt))}</span>`,
      status: `<tf-chip size="sm" dot status="${statusTone(a.status)}" label="${escapeAttr(T('approvals.status_' + a.status))}"></tf-chip>${
        a.decidedBy ? `<div class="tf-table__cell-sub">${escapeHtml(T('approvals.decided_by', { user: a.decidedBy }))}</div>` : ''}`,
    }));
  };

  table.rowActions = (row) => {
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
      b.addEventListener('click', (e) => { e.stopPropagation(); decide(a, act === 'approve'); });
      wrap.appendChild(b);
    }
    return wrap;
  };

  const paintSettings = () => {
    const s = state.settings;
    const el = body.querySelector('#nas-approvals-settings');
    if (!s) { el.textContent = ''; return; }
    const toggle = body.querySelector('#nas-approvals-enabled');
    if (toggle) toggle.checked = Boolean(s.enabled);
    const ttl = body.querySelector('#nas-approvals-ttl');
    // Only while the admin is not mid-edit: a poll must not overwrite what is
    // being typed.
    if (ttl && document.activeElement !== ttl) ttl.value = String(s.ttlHours);
    const origin = s.byDefault
      ? T(s.enabled ? 'approvals.settings_default_on' : 'approvals.settings_default_off', { n: s.adminCount })
      : T('approvals.settings_admins', { n: s.adminCount });
    el.innerHTML = `${escapeHtml(T('approvals.settings_sub'))} <span class="text-3">${escapeHtml(origin)}</span>${
      s.adminCount < 2 ? `<div class="text-3">${escapeHtml(T('approvals.single_admin'))}</div>` : ''}`;
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
    const detail = `${operationLabel(approval.operation)} — ${approval.subject}`;
    const note = await askDecision(approve, detail);
    if (note === null) return;
    // Approving RUNS the operation, so it needs the approver's own sudo
    // password in mode B; rejecting touches nothing on the node.
    const send = (sudoPassword) => screen.nas(
      'tentaNasApprovalDecideRequest',
      { requestId: approval.requestId, approve, note, sudoPassword },
      { timeoutMs: ADMIN_TIMEOUT_MS },
    );
    let res;
    try {
      res = approve
        ? await screen.withSudo(send, T('approvals.approve_title'))
        : await send(undefined);
    } catch (e) {
      toast(errMessage(e), 'error');
      refresh();
      return;
    }
    if (res === null) return;
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
        <div class="sr"><span class="k">${escapeHtml(T('approvals.col_subject'))}</span><span class="v">${escapeHtml(approval.subject)}</span></div>
      </div>
    </div>
    <div slot="footer">
      <tf-button variant="primary" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  win.addEventListener('action', () => win.close(true));
  return win;
}
