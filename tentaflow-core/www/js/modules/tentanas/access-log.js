// ===== File: modules/tentanas/access-log.js — the "Dziennik dostępu" of the Tasks tab (n15) and the forwarding settings of the alert pipeline (plan-02 §5.10/§5.9) =====
//
// What the node collected from `vfs_full_audit`, with the four filters §5.10
// names: user, share, operation, result. The filter values come from the
// ANSWER, not from a list in the browser — the node knows what it actually
// logged, and offering an operation nothing ever wrote would be a filter that
// can only return nothing.
//
// The card states two things the admin would otherwise have to discover: that
// an audited share which also serves SMB Direct is not audited on its RDMA
// path (§5.4b), and that an audited NFS export's events go to the HOST's audit
// log rather than into this table.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, fmtDate, fmtAgo, errMessage } from '/js/modules/tentanas/format.js';
import { setAttr, setText, patchHtml } from '/js/lib/dom-patch.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-window.js';

/** 'ok' | 'fail' as the node spells them, plus "both". */
const RESULTS = ['', 'ok', 'fail'];

const ANY = '__any';

/** The card the Tasks tab drops into its stack. `admin` gates the forwarding. */
export function accessLogCardHtml(admin) {
  return `
    <div class="section-card" id="nas-access-card">
      <div class="section-card-head">
        <div class="title">${sprite('file-text')} ${escapeHtml(T('access.title'))} <tf-chip size="sm" id="nas-access-count" label="0"></tf-chip></div>
        <div class="actions">
          <span class="hint" id="nas-access-hint"></span>
          ${admin ? `<tf-button size="sm" variant="ghost" icon="share" data-act="forward">${escapeHtml(T('access.forward_action'))}</tf-button>` : ''}
          <tf-button size="sm" variant="ghost" icon="refresh" data-act="refresh" title="${escapeAttr(T('access.refresh'))}"></tf-button>
        </div>
      </div>
      <div class="muted" id="nas-access-state"></div>
      <div class="row mt-sm">
        <tf-select id="nas-access-share" style="width:190px"></tf-select>
        <tf-select id="nas-access-user" style="width:190px"></tf-select>
        <tf-select id="nas-access-operation" style="width:190px"></tf-select>
        <tf-select id="nas-access-result" style="width:170px"></tf-select>
      </div>
      <tf-table id="nas-access-table" empty-message="${escapeAttr(T('access.none'))}">
        <tf-column key="at" label="${escapeAttr(T('access.col_at'))}" renderer="html" nowrap></tf-column>
        <tf-column key="user" label="${escapeAttr(T('access.col_user'))}" renderer="html" nowrap></tf-column>
        <tf-column key="share" label="${escapeAttr(T('access.col_share'))}" renderer="html" nowrap hide-below="900"></tf-column>
        <tf-column key="operation" label="${escapeAttr(T('access.col_operation'))}" renderer="html" nowrap></tf-column>
        <tf-column key="target" label="${escapeAttr(T('access.col_target'))}" renderer="html" fill></tf-column>
        <tf-column key="result" label="${escapeAttr(T('access.col_result'))}" renderer="html" nowrap></tf-column>
      </tf-table>
    </div>`;
}

/**
 * Wires the card written by `accessLogCardHtml`. Returns `{ refresh }`; the
 * caller polls it next to its other lists.
 */
export function wireAccessLog(screen, body) {
  const card = body.querySelector('#nas-access-card');
  const table = body.querySelector('#nas-access-table');
  const state = { filter: { share: '', user: '', operation: '', result: '' }, res: null };

  const paintFilters = (res) => {
    const fill = (id, key, values, labelOf) => {
      const el = body.querySelector(`#nas-access-${id}`);
      if (!el) return;
      const options = [
        { value: ANY, label: T('access.filter_any_' + id) },
        ...values.map((v) => ({ value: v, label: labelOf ? labelOf(v) : v })),
      ];
      // A value the node no longer has (its rows aged out) must not silently
      // keep filtering: the select falls back to "any" and so does the query.
      if (state.filter[key] && !values.includes(state.filter[key])) state.filter[key] = '';
      el.setOptions(options, state.filter[key] || ANY);
    };
    fill('share', 'share', res.shares || []);
    fill('user', 'user', res.users || []);
    fill('operation', 'operation', res.operations || []);
    fill('result', 'result', RESULTS.filter(Boolean), (v) => T('access.result_' + v));
  };

  const paint = () => {
    const res = state.res;
    if (!res) return;
    const audit = res.audit || {};
    const audited = audit.auditedShares || [];
    const exports_ = audit.auditedExports || [];
    // The card hides itself when nothing audits and nothing was ever logged:
    // an empty log plus a feature nobody switched on is noise.
    card.hidden = !audited.length && !exports_.length && !(res.total > 0);
    // Through the patch helpers: a bare `setAttribute` runs tf-chip's
    // `attributeChangedCallback` (and so re-renders the chip) even when the
    // value is identical, and a bare `textContent =` replaces the text node
    // under whatever the admin had selected.
    setAttr(body.querySelector('#nas-access-count'), 'label', String(Number(res.total) || 0));
    setText(body.querySelector('#nas-access-hint'), T('access.retention', { n: Number(audit.retentionDays) || 0 }));

    const shown = res.events || [];
    const lines = [];
    lines.push(audited.length
      ? T('access.audited_shares', { shares: audited.join(', ') })
      : T('access.audited_none'));
    if (exports_.length) lines.push(T('access.audited_exports', { shares: exports_.join(', ') }));
    if ((audit.unauditedSmbDirect || []).length) {
      lines.push(T('access.smb_direct_gap', { shares: audit.unauditedSmbDirect.join(', ') }));
    }
    if (audit.collectorState === 'unavailable') {
      lines.push(T('access.collector_unavailable', { detail: audit.detail || '' }));
    } else if (audit.detail) {
      lines.push(audit.detail);
    }
    if (audit.collectedAt) lines.push(T('access.collected', { t: fmtAgo(audit.collectedAt) }));
    // A reader who is not the organisation's admin gets no address at all
    // (the node sends none), only that forwarding is on and how it fares.
    const forward = res.forward || {};
    const targetOf = (f) => [f.syslogTarget, f.webhookUrl].filter(Boolean).join(', ');
    if (forward.enabled) {
      const target = targetOf(forward);
      lines.push(target
        ? T('access.forward_on', { target, n: Number(forward.pending) || 0 })
        : T('access.forward_on_hidden', { n: Number(forward.pending) || 0 }));
    }
    if (forward.lastError) lines.push(T('access.forward_error', { error: forwardErrorText(forward.lastError) }));
    const node = res.forwardNode || {};
    if (node.enabled) {
      const target = targetOf(node);
      lines.push(target
        ? T('access.forward_node_on', { target, n: Number(node.pending) || 0 })
        : T('access.forward_node_on_hidden', { n: Number(node.pending) || 0 }));
    }
    if (node.lastError) lines.push(T('access.forward_node_error', { error: forwardErrorText(node.lastError) }));
    // A page smaller than the match count has to say so, or the reader takes
    // the page for the whole answer. It belongs to the SAME block as the lines
    // above and is written with them in one go: the Tasks tab polls this card
    // every 30 s, and rebuilding the block destroyed every line whose text had
    // not changed. (It used to arrive through a second `innerHTML +=`, which
    // rebuilt the block twice per tick.)
    if (shown.length && Number(res.total) > shown.length) {
      lines.push(T('access.truncated', { shown: shown.length, total: Number(res.total) }));
    }
    patchHtml(body.querySelector('#nas-access-state'), lines.map((l) => `<div>${escapeHtml(l)}</div>`).join(''));

    table.rows = shown.map((e) => ({
      at: `<span class="tf-table__cell--mono">${escapeHtml(fmtDate(e.at))}</span>`,
      user: `<span class="tf-table__cell--mono">${escapeHtml(e.user || '—')}</span>${
        e.client ? `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(e.client)}</div>` : ''}`,
      share: `<span class="tf-table__cell--mono">${escapeHtml(e.share)}</span>`,
      operation: `<span class="tf-table__cell--mono">${escapeHtml(e.operation)}</span>`,
      target: `<span class="tf-table__cell--mono">${escapeHtml(e.target || '—')}</span>`,
      result: `<tf-chip size="sm" dot status="${e.result === 'fail' ? 'err' : 'ok'}" label="${escapeAttr(T('access.result_' + (e.result === 'fail' ? 'fail' : 'ok')))}"></tf-chip>${
        e.detail ? `<div class="tf-table__cell-sub">${escapeHtml(e.detail)}</div>` : ''}`,
    }));
  };

  const apply = (res) => { state.res = res; paintFilters(res); paint(); };

  const refresh = async () => {
    if (screen.disposed || !body.isConnected) return;
    try {
      const res = await screen.nas('tentaNasAccessLogRequest', { ...state.filter });
      if (screen.disposed || !body.isConnected) return;
      apply(res);
    } catch (e) {
      if (screen.disposed || !body.isConnected) return;
      toast(errMessage(e), 'error');
    }
  };

  for (const key of ['share', 'user', 'operation', 'result']) {
    body.querySelector(`#nas-access-${key}`)?.addEventListener('change', (e) => {
      const value = e.detail?.value;
      state.filter[key] = value === ANY ? '' : String(value || '');
      refresh();
    });
  }
  body.querySelector('#nas-access-card [data-act="refresh"]')?.addEventListener('click', refresh);
  body.querySelector('#nas-access-card [data-act="forward"]')?.addEventListener('click', () => {
    openForwardDialog(screen, state.res?.forward || {}, apply, state.res?.forwardNode || {});
  });

  return { refresh };
}

/**
 * A failed send as the node reports it: a neutral code, never a socket error
 * or an address (the node keeps those in its own log). Anything else — an
 * older node's raw text — reads as "not accepted" too.
 */
export function forwardErrorText(code) {
  const status = /^forward:http_status:(\d{3})$/.exec(String(code || ''))?.[1];
  if (status) return T('access.forward_err_http', { status });
  return T('access.forward_err_not_accepted');
}

/** A node-wide target that exists at all: switched on, or holding an address. */
function nodeTargetSet(node) {
  return Boolean(node && (node.enabled || node.syslogTarget || node.webhookUrl));
}

function targetFieldsHtml(prefix, t) {
  return `
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('access.forward_enabled'))}</span><span class="tc-sub">${escapeHtml(T('access.forward_enabled_sub'))}</span></div>
        <tf-toggle id="${prefix}-enabled" ${t.enabled ? 'checked' : ''}></tf-toggle>
      </div>
      <tf-input id="${prefix}-syslog" label="${escapeAttr(T('access.forward_syslog'))}" placeholder="siem.example.com:514" autocomplete="off" spellcheck="false" value="${escapeAttr(t.syslogTarget || '')}" hint="${escapeAttr(T('access.forward_syslog_hint'))}"></tf-input>
      <tf-input id="${prefix}-webhook" label="${escapeAttr(T('access.forward_webhook'))}" placeholder="https://siem.example.com/hooks/tentanas" autocomplete="off" spellcheck="false" value="${escapeAttr(t.webhookUrl || '')}" hint="${escapeAttr(T('access.forward_webhook_hint'))}"></tf-input>
      ${t.webhookNeedsMigration ? `<div class="wizard-warning warning" id="${prefix}-webhook-migration">${escapeHtml(T('access.forward_webhook_migration'))}</div>` : ''}`;
}

function readTarget(win, prefix) {
  return {
    enabled: Boolean(win.querySelector(`#${prefix}-enabled`).checked),
    syslogTarget: String(win.querySelector(`#${prefix}-syslog`).value || '').trim(),
    webhookUrl: String(win.querySelector(`#${prefix}-webhook`).value || '').trim(),
  };
}

// The retired node-wide target (owner decision, wave 9b round 2): shown as
// it is — masked — and deletable, never editable.
function nodeTargetHtml(node) {
  const target = [node.syslogTarget, node.webhookUrl].filter(Boolean).join(', ');
  return `
      <div class="section-title mt-md">${escapeHtml(T('access.forward_node_title'))}</div>
      <div class="hint">${escapeHtml(T('access.forward_node_scope'))}</div>
      <div class="toggle-card">
        <div class="tc-text"><span class="mono">${escapeHtml(target || '—')}</span><span class="tc-sub">${escapeHtml(T(node.enabled ? 'access.forward_node_state_on' : 'access.forward_node_state_off'))}</span></div>
        <tf-button size="sm" variant="danger" icon="trash" data-act="delete-node">${escapeHtml(T('access.forward_node_delete'))}</tf-button>
      </div>`;
}

/**
 * Where the alerts go (§5.9), one target per organisation (wave 9b).
 *
 * The first section is the admin's OWN organisation's target, set once for
 * the whole fleet: it receives that organisation's alerts, the node-wide
 * alerts every organisation sees (disks, pools) and — when switched on — the
 * access log of that organisation's own shares. Never another tenant's row
 * (`tentanas/forward.rs`).
 *
 * The second section appears only while the RETIRED node-wide target from
 * before exists: it keeps sending node-wide alerts only, as it always did,
 * it is shown masked and it can only be deleted — nothing edits it or
 * creates a new one (owner decision, wave 9b). The organisation's own target
 * already receives the node-wide alerts.
 *
 * A webhook URL is a secret: the node sends it masked (scheme and host), the
 * field shows that mask, and a save that leaves it untouched keeps the stored
 * URL.
 *
 * Both targets are optional and independent; the node refuses an address it
 * could not use, so the dialog shows that error instead of saving something
 * that would fail silently every minute.
 */
export function openForwardDialog(screen, forward, onSaved, node = {}) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('access.forward_title'));
  win.setAttribute('icon', 'share');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '600');
  win.setAttribute('min-width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const withNode = nodeTargetSet(node);
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="explain-box">${escapeHtml(T('access.forward_explain'))}</div>
      <div class="section-title">${escapeHtml(T('access.forward_org_title'))}</div>
      <div class="hint">${escapeHtml(T('access.forward_org_scope'))}</div>
      ${targetFieldsHtml('nas-forward', forward)}
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('access.forward_include'))}</span><span class="tc-sub">${escapeHtml(T('access.forward_include_sub'))}</span></div>
        <tf-toggle id="nas-forward-include" ${forward.includeAccess ? 'checked' : ''}></tf-toggle>
      </div>
      ${withNode ? nodeTargetHtml(node) : ''}
      <div class="num-err" id="nas-forward-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(T('schedule.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  let busy = false;
  win.querySelector('[data-act="delete-node"]')?.addEventListener('click', async () => {
    if (busy) return;
    busy = true;
    try {
      // Deleting is the one change the retired target takes: off, no address.
      const res = await screen.nas('tentaNasAlertForwardSetRequest', { enabled: false, syslogTarget: '', webhookUrl: '', includeAccess: false, nodeWide: true });
      toast(T('access.forward_node_deleted'), 'success');
      win.close(true);
      if (onSaved) onSaved(res);
    } catch (err) {
      busy = false;
      const errEl = win.querySelector('#nas-forward-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    busy = true;
    try {
      // The webhook field holds the MASKED address the node sent; sent back
      // unchanged it keeps the stored one, and the secret never travels.
      const res = await screen.nas('tentaNasAlertForwardSetRequest', {
        ...readTarget(win, 'nas-forward'),
        includeAccess: Boolean(win.querySelector('#nas-forward-include').checked),
        nodeWide: false,
      });
      toast(T('access.forward_saved'), 'success');
      win.close(true);
      if (onSaved) onSaved(res);
    } catch (err) {
      busy = false;
      const errEl = win.querySelector('#nas-forward-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}
