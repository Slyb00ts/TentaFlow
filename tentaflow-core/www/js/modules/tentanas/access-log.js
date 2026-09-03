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
    body.querySelector('#nas-access-count').setAttribute('label', String(Number(res.total) || 0));
    body.querySelector('#nas-access-hint').textContent = T('access.retention', { n: Number(audit.retentionDays) || 0 });

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
    const forward = res.forward || {};
    if (forward.enabled) {
      lines.push(T('access.forward_on', {
        target: [forward.syslogTarget, forward.webhookUrl].filter(Boolean).join(', '),
        n: Number(forward.pending) || 0,
      }));
    }
    if (forward.lastError) lines.push(T('access.forward_error', { error: forward.lastError }));
    const stateEl = body.querySelector('#nas-access-state');
    stateEl.innerHTML = lines.map((l) => `<div>${escapeHtml(l)}</div>`).join('');

    const shown = res.events || [];
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
    // A page smaller than the match count has to say so, or the reader takes
    // the page for the whole answer.
    if (shown.length && Number(res.total) > shown.length) {
      stateEl.innerHTML += `<div>${escapeHtml(T('access.truncated', { shown: shown.length, total: Number(res.total) }))}</div>`;
    }
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
    openForwardDialog(screen, state.res?.forward || {}, apply);
  });

  return { refresh };
}

/**
 * Where this node sends its alerts and (optionally) its access log (§5.9).
 * Both targets are optional and independent; the node refuses a target it
 * could not use, so the dialog reports its error instead of saving something
 * that would fail silently every two minutes.
 */
export function openForwardDialog(screen, forward, onSaved) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('access.forward_title'));
  win.setAttribute('icon', 'share');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '580');
  win.setAttribute('min-width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="explain-box">${escapeHtml(T('access.forward_explain'))}</div>
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('access.forward_enabled'))}</span><span class="tc-sub">${escapeHtml(T('access.forward_enabled_sub'))}</span></div>
        <tf-toggle id="nas-forward-enabled" ${forward.enabled ? 'checked' : ''}></tf-toggle>
      </div>
      <tf-input id="nas-forward-syslog" label="${escapeAttr(T('access.forward_syslog'))}" placeholder="siem.example.com:514" autocomplete="off" spellcheck="false" value="${escapeAttr(forward.syslogTarget || '')}" hint="${escapeAttr(T('access.forward_syslog_hint'))}"></tf-input>
      <tf-input id="nas-forward-webhook" label="${escapeAttr(T('access.forward_webhook'))}" placeholder="https://siem.example.com/hooks/tentanas" autocomplete="off" spellcheck="false" value="${escapeAttr(forward.webhookUrl || '')}" hint="${escapeAttr(T('access.forward_webhook_hint'))}"></tf-input>
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('access.forward_include'))}</span><span class="tc-sub">${escapeHtml(T('access.forward_include_sub'))}</span></div>
        <tf-toggle id="nas-forward-include" ${forward.includeAccess ? 'checked' : ''}></tf-toggle>
      </div>
      <div class="num-err" id="nas-forward-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(T('schedule.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    busy = true;
    try {
      const res = await screen.nas('tentaNasAlertForwardSetRequest', {
        enabled: Boolean(win.querySelector('#nas-forward-enabled').checked),
        syslogTarget: String(win.querySelector('#nas-forward-syslog').value || '').trim(),
        webhookUrl: String(win.querySelector('#nas-forward-webhook').value || '').trim(),
        includeAccess: Boolean(win.querySelector('#nas-forward-include').checked),
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
