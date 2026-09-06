// ===== File: modules/tentanas/targets.js — the block targets of the Sharing tab (n12): the targets table, the target detail with the initiator allowlist and the configfs preview, pause / resume and delete =====
//
// One list request feeds the section: the targets of this node plus what this
// node can actually serve (LIO, nvmet, iSER, NVMe-oF over RDMA,
// DH-HMAC-CHAP), the interfaces the portal picker offers and the zvols that
// are still free.
//
// Everything in here repeats one fact, because it is the fact the plan asks to
// be repeated (§5.5): an IQN/NQN is declared by the CLIENT, so the allowlist
// is a filter and not a login, and a block export hands out a raw disk with no
// file ACLs.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, ADMIN_TIMEOUT_MS, fmtBytes, fmtAgo, errMessage } from '/js/modules/tentanas/format.js';
import { openRetypeDialog, followResponse, warningHtml } from '/js/modules/tentanas/dialogs.js';
import { openTargetWizard, sharedHostWarning, parseHostNqns, invalidHostNqns } from '/js/modules/tentanas/target-wizard.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-window.js';
import '/js/components/tf-input.js';

export const protocolLabel = (protocol) => (protocol === 'nvmet' ? 'NVMe-oF' : 'iSCSI');
export const protocolChipHtml = (protocol) => `<tf-chip size="sm" status="${protocol === 'nvmet' ? 'accent' : 'info'}" label="${escapeAttr(protocolLabel(protocol))}"></tf-chip>`;

const AUTH_LABEL = {
  none: 'targets.auth_none',
  chap: 'targets.auth_chap',
  'mutual-chap': 'targets.auth_mutual_chap',
  dhchap: 'targets.auth_dhchap',
  'dhchap-bidi': 'targets.auth_dhchap_bidi',
};
export const authLabel = (method) => T(AUTH_LABEL[method] || 'targets.auth_none');

/**
 * The "Uwierzytelnienie" cell of n12. `none` is the only one that is not a
 * green lock: an unauthenticated target is reachable by whoever gets to the
 * portal, whatever the allowlist says.
 */
export function authChipHtml(auth) {
  const method = auth?.method || 'none';
  if (method === 'none') {
    return `<tf-chip size="sm" status="warn" icon="alert" label="${escapeAttr(T('targets.auth_none_warning'))}"></tf-chip>`;
  }
  return `<tf-chip size="sm" status="ok" icon="lock" label="${escapeAttr(authLabel(method))}"></tf-chip>`;
}

const TRANSPORT_LABEL = { tcp: 'targets.transport_tcp', iser: 'targets.transport_iser', rdma: 'targets.transport_rdma' };
export const transportLabel = (transport) => T(TRANSPORT_LABEL[transport] || 'targets.transport_tcp');

/**
 * The "Portal / transport" cell: the address the kernel actually binds, and
 * underneath it the interface the admin picked — or the fact that no interface
 * was picked at all, which is the case the wizard makes you confirm.
 *
 * n12 prints only ONE of the two on its second line (the iSCSI row shows
 * "iface storage0", the NVMe-oF row "TCP + RDMA (RoCE)"); this prints both.
 * Deliberate: the interface is the half the portal-drift alert is about, and a
 * row that shows an address without saying which interface it was picked on
 * gives an admin nothing to compare the alert against.
 */
export function portalCellHtml(target) {
  const portals = target.portals || [];
  if (!portals.length) return '—';
  const address = `${portals[0].address}:${portals[0].port}`;
  const iface = portals[0].interface
    ? T('targets.iface', { name: portals[0].interface })
    : T('targets.all_interfaces');
  const transports = [...new Set(portals.map((p) => transportLabel(p.transport)))].join(' + ');
  return `<span class="tf-table__cell--mono">${escapeHtml(address)}</span><div class="tf-table__cell-sub">${escapeHtml(iface)} · ${escapeHtml(transports)}</div>`;
}

export function sourceCellHtml(target) {
  const lun = (target.luns || [])[0];
  if (!lun) return '—';
  const size = [fmtBytes(lun.sizeBytes), lun.thin ? 'thin' : ''].filter(Boolean).join(' · ');
  return `<span class="tf-table__cell--mono">zvol ${escapeHtml(lun.source)}</span><div class="tf-table__cell-sub">${escapeHtml(size)}</div>`;
}

/**
 * The count next to "Zalogowane initiatory", and the sentence under it when
 * the list is empty.
 *
 * `sessionsKnown` is the whole reason these are functions: for NVMe-oF the
 * node may be unable to measure at all (nvmet publishes its controllers in
 * debugfs, not configfs), and a confident "0" would be a lie in exactly the
 * place — the delete dialog's blast radius — where it costs a client its disk
 * mid-write. Unknown reads as a dash with the reason, never as zero.
 */
// `!== true`, not `=== false`: the safe default is "unknown". The node's own
// field is `#[serde(default)]`, so a response built before this field existed —
// or one that lost it in transit — arrives with nothing there, and printing a
// confident "0" for it is exactly the measured zero the whole path exists to
// avoid. A dash asks the admin to look; a zero tells them not to.
export const sessionsCountLabel = (t) => (t.sessionsKnown !== true ? '—' : String((t.sessions ?? 0)));
export const sessionsEmptyText = (t) => T(t.sessionsKnown !== true ? 'targets.sessions_unknown' : 'targets.sessions_none');

/**
 * The one place a target's state becomes a chip.
 *
 * Shared by the table and the detail window, which used to render the same row
 * two different ways one click apart: an unauthenticated `active` target was a
 * yellow "warning" in the table and a green "active" in the window. `''` means
 * "nothing worth a chip" — a clean active row in the table draws none, and the
 * detail window, which always wants one, supplies the green one itself.
 */
function stateChip(t) {
  if (t.state === 'error') {
    return `<span title="${escapeAttr(t.stateDetail || '')}"><tf-chip size="sm" status="err" dot label="${escapeAttr(T('targets.state_error'))}"></tf-chip></span>`;
  }
  if (!t.enabled || t.state === 'disabled') {
    return `<tf-chip size="sm" status="neutral" label="${escapeAttr(T('targets.state_disabled'))}"></tf-chip>`;
  }
  // The node has decided this target should be exported and is not exporting
  // it yet — saved seconds ago, a pool still importing, a reconcile that has
  // not come round. Nothing is wrong, so it is not an error; but it is not
  // ACTIVE either, and a green chip over an empty kernel is exactly how a
  // target lost to a transient used to sit "active" forever with nothing
  // behind it.
  if (t.state === 'pending') {
    return `<span title="${escapeAttr(t.stateDetail || '')}"><tf-chip size="sm" status="info" dot label="${escapeAttr(T('targets.state_pending'))}"></tf-chip></span>`;
  }
  // An ACTIVE target with a detail is the node saying the export works and is
  // not authenticated — never silent, never the same chip as a clean one.
  if (t.stateDetail) {
    return `<span title="${escapeAttr(t.stateDetail)}"><tf-chip size="sm" status="warn" dot label="${escapeAttr(T('targets.state_warning'))}"></tf-chip></span>`;
  }
  return '';
}

export function targetRow(t) {
  return {
    _target: t,
    name: `<div class="tf-table__cell-row">${sprite('target')}<span class="tf-table__cell--mono"><span class="tf-table__cell-title tf-table__cell-title--strong">${escapeHtml(t.name)}</span></span>${stateChip(t)}</div>${t.stateDetail ? `<div class="tf-table__cell-sub">${escapeHtml(t.stateDetail)}</div>` : ''}`,
    protocol: protocolChipHtml(t.protocol),
    source: sourceCellHtml(t),
    auth: authChipHtml(t.auth),
    portal: portalCellHtml(t),
  };
}

// ---------------------------------------------------------------------------
// The section of the Sharing tab
// ---------------------------------------------------------------------------

/**
 * Draws the "Targety blokowe" card into `host` and returns a controller the
 * Sharing tab drives: `render(state)` repaints from the last list answer,
 * `reload()` asks for a fresh one.
 */
export function mountTargetsSection(screen, host, { onChange = null } = {}) {
  host.innerHTML = `
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('target')} ${escapeHtml(T('targets.title'))} <tf-chip size="sm" status="neutral" id="nas-tg-count" label="0"></tf-chip></div>
        <span class="hint">${escapeHtml(T('targets.hint'))}</span>
      </div>
      <div id="nas-tg-list"></div>
      <div id="nas-tg-services"></div>
    </div>`;

  const state = { targets: [], capabilities: null, services: [], filter: 'all', query: '', error: '' };

  const guardAdmin = () => {
    if (screen.isAdmin) return true;
    toast(T('elevation.admin_only'), 'warning');
    return false;
  };
  const refresh = () => { if (onChange) onChange(); };
  const openCreate = () => {
    if (!guardAdmin()) return;
    openTargetWizard(screen, { capabilities: state.capabilities, targets: state.targets, onDone: refresh });
  };
  // The pencil opens the WIZARD, which is what n12 says it does. The detail
  // window is one row-click away and carries the things the wizard does not
  // (the iSCSI allowlist, the rendered configfs plan), so both surfaces are
  // reachable in one click instead of the wizard being two.
  const openEdit = (target) => {
    if (!guardAdmin()) return;
    openTargetWizard(screen, { target, capabilities: state.capabilities, targets: state.targets, onDone: refresh });
  };

  const visible = () => state.targets.filter((t) => {
    if (state.filter !== 'all' && t.protocol !== state.filter) return false;
    if (!state.query) return true;
    const q = state.query;
    return t.name.toLowerCase().includes(q)
      || (t.wwn || '').toLowerCase().includes(q)
      || (t.luns || []).some((l) => (l.source || '').toLowerCase().includes(q));
  });

  const paint = () => {
    // The kernel side of each protocol, when it is not there. A node that
    // cannot serve NVMe-oF says so here instead of only inside the wizard.
    host.querySelector('#nas-tg-services').innerHTML = state.services
      .filter((s) => !s.installed)
      .map((s) => `<div class="muted">${escapeHtml(T('targets.service_missing', { proto: protocolLabel(s.protocol), detail: s.detail }))}</div>`)
      .join('');
    const list = host.querySelector('#nas-tg-list');
    host.querySelector('#nas-tg-count').setAttribute('label', String(state.targets.length));
    if (state.error && !state.targets.length) {
      list.innerHTML = `<div class="num-err">${escapeHtml(state.error)}</div>`;
      return;
    }
    if (!state.targets.length) {
      list.innerHTML = `
        <tf-empty-state icon="target" title="${escapeAttr(T('targets.empty_title'))}" message="${escapeAttr(T('targets.empty_msg'))}">
          ${screen.isAdmin ? `<tf-button variant="secondary" icon="plus" data-act="create-empty">${escapeHtml(T('targets.create'))}</tf-button>` : ''}
        </tf-empty-state>`;
      list.querySelector('[data-act="create-empty"]')?.addEventListener('click', openCreate);
      return;
    }
    let table = list.querySelector('#nas-tg-table');
    if (!table) {
      list.innerHTML = `
        <tf-table id="nas-tg-table" actions-label="${escapeAttr(I18n.t('common.actions'))}" empty-message="${escapeAttr(T('targets.none_match'))}">
          <tf-column key="name" label="${escapeAttr(T('targets.col_name'))}" renderer="html" fill></tf-column>
          <tf-column key="protocol" label="${escapeAttr(T('targets.col_protocol'))}" renderer="html" nowrap></tf-column>
          <tf-column key="source" label="${escapeAttr(T('targets.col_source'))}" renderer="html" hide-below="900"></tf-column>
          <tf-column key="auth" label="${escapeAttr(T('targets.col_auth'))}" renderer="html" nowrap></tf-column>
          <tf-column key="portal" label="${escapeAttr(T('targets.col_portal'))}" renderer="html" hide-below="1000"></tf-column>
        </tf-table>`;
      table = list.querySelector('#nas-tg-table');
      table.rowActions = (row) => {
        const t = row._target;
        const wrap = document.createElement('div');
        wrap.className = 'tf-table__cell-row';
        wrap.innerHTML = screen.isAdmin ? `
          <tf-button size="sm" variant="ghost" icon="edit" data-act="edit" title="${escapeAttr(T('targets.edit'))}"></tf-button>
          <tf-button size="sm" variant="ghost" icon="${t.enabled ? 'pause' : 'play'}" data-act="pause" title="${escapeAttr(t.enabled ? T('targets.pause') : T('targets.resume'))}"></tf-button>
          <tf-button size="sm" variant="ghost" tone="critical" icon="trash" data-act="delete" title="${escapeAttr(T('targets.delete'))}"></tf-button>`
          : `<tf-button size="sm" variant="ghost" icon="eye" data-act="details" title="${escapeAttr(T('targets.details'))}"></tf-button>`;
        wrap.querySelector('[data-act="details"]')?.addEventListener('click', (e) => { e.stopPropagation(); openTargetDetail(screen, t.targetId, { capabilities: state.capabilities, siblings: state.targets, onChange: refresh }); });
        wrap.querySelector('[data-act="edit"]')?.addEventListener('click', (e) => { e.stopPropagation(); openEdit(t); });
        wrap.querySelector('[data-act="pause"]')?.addEventListener('click', (e) => { e.stopPropagation(); setTargetEnabled(screen, t, !t.enabled, refresh); });
        wrap.querySelector('[data-act="delete"]')?.addEventListener('click', (e) => { e.stopPropagation(); openTargetDeleteDialog(screen, t, refresh); });
        return wrap;
      };
      table.addEventListener('row-click', (e) => openTargetDetail(screen, e.detail.row._target.targetId, { capabilities: state.capabilities, siblings: state.targets, onChange: refresh }));
    }
    table.rows = visible().map(targetRow);
    // A drift alert names its target, and the button that follows it lands
    // here. Landing at the top of a table of twenty is not landing on the
    // thing the alert was about — so the name arrives with the navigation and
    // that target's window opens on top of the list, the same way an alert
    // about a disk opens that disk. The table is still behind it, which is the
    // half §5.5 asks for: the admin sees whether anything else drifted too.
    //
    // Consumed once. A name left lying around would reopen the window every
    // time the list repainted.
    const wanted = screen.targetName;
    if (wanted) {
      screen.targetName = null;
      const row = state.targets.find((t) => t.name === wanted);
      if (row) {
        openTargetDetail(screen, row.targetId, {
          capabilities: state.capabilities,
          siblings: state.targets,
          onChange: refresh,
        });
      }
    }
  };

  return {
    state,
    openCreate,
    set(answer) {
      state.targets = (answer.targets || []).slice().sort((a, b) => a.name.localeCompare(b.name));
      state.capabilities = answer.capabilities || null;
      state.services = answer.services || [];
      state.error = '';
      paint();
    },
    fail(message) {
      state.error = message;
      paint();
    },
    filter(value) { state.filter = value; paint(); },
    search(value) { state.query = (value || '').trim().toLowerCase(); paint(); },
  };
}

/**
 * "Zatrzymaj target": the target keeps everything it has, only `enabled`
 * flips — and the node takes it back out of the kernel, because a paused
 * target that still exports a disk would be a lie.
 */
export async function setTargetEnabled(screen, target, enabled, onDone) {
  const title = enabled ? T('targets.resume_title', { name: target.name }) : T('targets.pause_title', { name: target.name });
  const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasTargetUpdateRequest', {
    targetId: target.targetId,
    // NO portals, and no `repickPortal`. Pausing a target is not a request to
    // move its portal, and sending one used to be exactly that: the node
    // re-derived the address from the interface on every save, so one click on
    // "Wznów" could rebind a drifted export onto a network nobody picked — and
    // on an interface with two addresses it moved a LIVE portal, cutting off
    // every initiator logged in on the old one (owner decision 2026-09-04).
    portals: [],
    auth: target.auth || null,
    initiators: target.initiators || [],
    portGroups: target.portGroups || [],
    // Already stored as such; re-confirming keeps a paused 0.0.0.0 target from
    // being refused on resume.
    confirmAllInterfaces: (target.portals || []).some((p) => !p.interface),
    enabled,
    sudoPassword,
  }, { timeoutMs: ADMIN_TIMEOUT_MS }), title);
  followResponse(screen, res, onDone, enabled ? T('targets.resumed_done', { name: target.name }) : T('targets.paused_done', { name: target.name }));
}

// ---------------------------------------------------------------------------
// Target detail: the allowlist, the port groups and the rendered configfs
// ---------------------------------------------------------------------------

const GROUP_STATE_LABEL = {
  optimized: 'targets.group_optimized',
  'non-optimized': 'targets.group_non_optimized',
  unavailable: 'targets.group_unavailable',
  transitioning: 'targets.group_transitioning',
};
/// A state this build does not know is shown AS IT IS. Falling back to
/// "Active/Optimized" would report the most optimistic possible reading of a
/// path whose real state we could not name.
export const groupStateLabel = (state) => (GROUP_STATE_LABEL[state] ? T(GROUP_STATE_LABEL[state]) : String(state || '—'));

/**
 * One line of "Zalogowane initiatory": both halves of a session, the way the
 * share detail shows them in its two columns.
 *
 * `client` is WHERE the session came from — an address — and `user` is the
 * identity it declared (an initiator IQN, a host NQN). For NVMe-oF they
 * differ, because nvmet publishes `host_traddr` next to `hostnqn`, and the
 * difference is the point §5.5 keeps making: the NQN is a string the client
 * picks for itself, the address is not. For iSCSI the two are the same string
 * and only one is printed.
 */
export const sessionLine = (s) => (s.user && s.user !== s.client
  ? `${escapeHtml(s.client)} · ${escapeHtml(s.user)}`
  : escapeHtml(s.client || s.user || '—'));

/**
 * One IQN/NQN per line; blanks and duplicates fall away.
 *
 * Delegates to the wizard's parser rather than repeating it: the two used to
 * differ — the wizard lower-cased, this did not — so the same paste produced
 * two different allowlists depending on which window the admin happened to
 * open. Neither lower-cases now (an NQN is matched with `strcmp`), and there
 * is one rule instead of two.
 */
export const parseInitiators = (text) => parseHostNqns(text);

/**
 * `siblings` is the node's other targets. The detail window needs them for the
 * same reason the wizard does: an nvmet host NQN is a NODE-WIDE object that
 * carries the DH-HMAC-CHAP key, so adding one here can collide with another
 * target — and the node refuses such a save. Without the list this window
 * showed no warning at all, and it is the surface an admin edits an allowlist
 * from.
 */
export function openTargetDetail(screen, targetId, { capabilities = null, siblings = [], onChange = null } = {}) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('targets.detail_title'));
  win.setAttribute('icon', 'target');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '800');
  win.setAttribute('min-width', '600');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `<div slot="body" class="stack"><div class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>
    <div slot="footer"><tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button></div>`;
  document.body.appendChild(win);
  const state = { target: null, sessions: [], preview: '', initiatorsText: '' };

  const load = async () => {
    try {
      const r = await screen.nas('tentaNasTargetGetRequest', { targetId });
      state.target = r.target;
      state.sessions = r.sessions || [];
      state.preview = r.configPreview || '';
      state.initiatorsText = (r.target.initiators || []).join('\n');
    } catch (e) {
      if (win.isConnected) win.querySelector('[slot="body"]').innerHTML = `<div class="num-err">${escapeHtml(errMessage(e))}</div>`;
      return false;
    }
    if (win.isConnected) draw();
    return true;
  };

  // The same node-wide-host warning the wizard shows, on the other surface an
  // allowlist is edited from — and with the SAME sentence-picking rule.
  // Hard-coding `dhchap_hosts_shared` here advised "set the same key here" in
  // a window that has no key field, on targets that have no key: this passes
  // the target's own method to the one function that chooses.
  const sharedWarningHtml = (t) => {
    const shared = sharedHostWarning(
      siblings,
      t.protocol,
      parseInitiators(state.initiatorsText),
      t.targetId,
      // The whole `auth`, not just the method: a saved row that says `dhchap`
      // with no stored secret holds nothing on the shared object, and the
      // server skips it for that reason too.
      t.auth,
    );
    // The wizard's own amber block, spelled the same way: the base
    // `.wizard-warning` (no modifier) with the `alert` icon. `warningHtml`
    // only knows `info` and `danger`, and neither is what this is.
    const sharedHtml = shared
      ? `<div class="wizard-warning">${sprite('alert')}<div>${escapeHtml(T(shared.key, { nqns: shared.nqns, targets: shared.targets }))}</div></div>`
      : '';
    // The shape check the wizard has, on the surface that did not: this window
    // can save an allowlist too, and an NQN the node refuses came back as a
    // raw catalog string after the sudo prompt. nvmet only — an iSCSI ACL is
    // an IQN and has its own alphabet.
    const invalid = t.protocol === 'nvmet' ? invalidHostNqns(state.initiatorsText) : [];
    const invalidHtml = invalid.length
      ? `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_target.host_nqn_invalid', { nqns: invalid.join(', ') }))}</div></div>`
      : '';
    return sharedHtml + invalidHtml;
  };

  const draw = () => {
    const t = state.target;
    win.setAttribute('subtitle', `${t.name} · ${protocolLabel(t.protocol)}`);
    const lun = (t.luns || [])[0];
    const groups = (t.portGroups || []).map((g) => `<div class="sr"><span class="k">${escapeHtml(T('targets.port_group_row', { n: g.groupId }))}</span><span class="v">${escapeHtml(groupStateLabel(g.state))}${g.preferred ? ` · ${escapeHtml(T('targets.group_preferred'))}` : ''}</span></div>`).join('');
    win.innerHTML = `
      <div slot="body" class="stack">
        <div class="row">
          ${protocolChipHtml(t.protocol)}
          ${authChipHtml(t.auth)}
          ${stateChip(t) || `<tf-chip size="sm" status="ok" dot label="${escapeAttr(T('targets.state_active'))}"></tf-chip>`}
          ${t.stateDetail ? `<span class="text-3">${escapeHtml(t.stateDetail)}</span>` : ''}
        </div>
        <div class="stat-rows">
          <div class="sr"><span class="k">${escapeHtml(T('targets.wwn'))}</span><span class="v mono">${escapeHtml(t.wwn)}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('targets.lun'))}</span><span class="v mono">${lun ? `${escapeHtml(lun.source)} · ${escapeHtml(fmtBytes(lun.sizeBytes))}${lun.thin ? ' · thin' : ''}` : '—'}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('targets.portal'))}</span><span class="v">${portalCellHtml(t)}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('targets.col_auth'))}</span><span class="v">${escapeHtml(authLabel(t.auth?.method))}${t.auth?.username ? ` · <span class="mono">${escapeHtml(t.auth.username)}</span>` : ''}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('targets.created'))}</span><span class="v">${escapeHtml(t.createdAt ? fmtAgo(t.createdAt) : '—')}</span></div>
        </div>
        <div class="section-card-head"><div class="title">${sprite('layers')} ${escapeHtml(T('targets.port_groups'))}</div></div>
        <div class="stat-rows" id="nas-td-groups">${groups}</div>
        <div class="section-card-head"><div class="title">${sprite('shield')} ${escapeHtml(T('targets.initiators'))}</div></div>
        ${screen.isAdmin ? `
          <tf-input id="nas-td-initiators" multiline rows="3" spellcheck="false" hint="${escapeAttr(T('targets.initiators_hint'))}" value="${escapeAttr(state.initiatorsText)}"></tf-input>`
          : `<div class="mono">${(t.initiators || []).map((i) => escapeHtml(i)).join('<br>') || escapeHtml(T('targets.no_initiators'))}</div>`}
        ${(t.initiators || []).length ? '' : `<div class="muted">${escapeHtml(T('targets.no_initiators'))}</div>`}
        <div id="nas-td-shared">${sharedWarningHtml(t)}</div>
        ${warningHtml('info', T('targets.allowlist_note'))}
        ${warningHtml('danger', T('targets.raw_disk_note'))}
        <div class="section-card-head"><div class="title">${sprite('users')} ${escapeHtml(T('targets.sessions_title'))} <tf-chip size="sm" status="neutral" label="${escapeAttr(sessionsCountLabel(t))}"></tf-chip></div></div>
        ${state.sessions.length
          ? `<div class="mono" id="nas-td-sessions">${state.sessions.map(sessionLine).join('<br>')}</div>`
          : `<div class="muted">${escapeHtml(sessionsEmptyText(t))}</div>`}
        <div class="section-card-head"><div class="title">${sprite('terminal')} ${escapeHtml(T('targets.config_preview'))}</div><span class="hint">${escapeHtml(T('targets.config_preview_hint'))}</span></div>
        <pre class="cmd" id="nas-td-preview">${escapeHtml(state.preview)}</pre>
      </div>
      <div slot="footer">
        ${screen.isAdmin ? `<tf-button variant="ghost" tone="critical" icon="trash" data-act="delete">${escapeHtml(T('targets.delete'))}</tf-button>
        <tf-button variant="ghost" icon="${t.enabled ? 'pause' : 'play'}" data-act="pause">${escapeHtml(t.enabled ? T('targets.pause') : T('targets.resume'))}</tf-button>` : ''}
        <span class="spacer"></span>
        <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button>
        ${screen.isAdmin ? `<tf-button variant="secondary" icon="edit" data-act="edit">${escapeHtml(T('targets.edit'))}</tf-button>
        <tf-button variant="primary" icon="check" data-act="save">${escapeHtml(T('targets.save'))}</tf-button>` : ''}
      </div>`;
    win.querySelector('#nas-td-initiators')?.addEventListener('input', (e) => {
      state.initiatorsText = e.target.value;
      // The shared-host warning follows what is typed, and only it: the whole
      // window is NOT repainted, because that would take the focus out of the
      // field on every keystroke.
      const box = win.querySelector('#nas-td-shared');
      if (!box) return;
      box.innerHTML = sharedWarningHtml(t);
    });
    win.querySelector('[data-act="save"]')?.addEventListener('click', () => saveAllowlist());
    win.querySelector('[data-act="edit"]')?.addEventListener('click', () => {
      win.close(true);
      // The node's real target list, not `[t]`: `sharedHostTargets` excludes
      // the target being edited, so a one-element list always filtered to
      // empty and this path — the ordinary way to edit an existing target —
      // showed no shared-host warning at all.
      openTargetWizard(screen, { target: t, capabilities, targets: siblings, onDone: onChange });
    });
    win.querySelector('[data-act="delete"]')?.addEventListener('click', () => {
      win.close(true);
      openTargetDeleteDialog(screen, t, onChange);
    });
    win.querySelector('[data-act="pause"]')?.addEventListener('click', async () => {
      await setTargetEnabled(screen, t, !t.enabled, onChange);
      if (win.isConnected) load();
    });
  };

  const saveAllowlist = async () => {
    const t = state.target;
    if (!t.auth) {
      // NEVER send `null` here. `target_auth_columns` reads a missing `auth`
      // as "the admin chose no authentication" and wipes every stored secret —
      // so an allowlist edit would silently turn an authenticated target into
      // an open one. `to_protocol` always fills this in, so this is a guard
      // against a future response shape, not a case seen today; it fails loudly
      // instead of downgrading.
      toast(T('targets.save_auth_missing'), 'error');
      return;
    }
    // The same gate the wizard puts on its Next button. The amber block below
    // the field NAMES a malformed NQN, but naming it and then sending it
    // anyway leaves the admin with a raw catalog string after the sudo prompt
    // — one list, two surfaces, two rules, which is the shape that keeps
    // coming back on this pair of windows.
    const badNqns = t.protocol === 'nvmet' ? invalidHostNqns(state.initiatorsText) : [];
    if (badNqns.length) {
      toast(T('wizard_target.host_nqn_invalid', { nqns: badNqns.join(', ') }), 'error');
      return;
    }
    const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasTargetUpdateRequest', {
      targetId,
      // Saving the allowlist changes the allowlist. The portal stays where the
      // admin put it — see `setTargetEnabled` for what sending it used to do.
      portals: [],
      auth: t.auth,
      initiators: parseInitiators(state.initiatorsText),
      portGroups: t.portGroups || [],
      confirmAllInterfaces: (t.portals || []).some((p) => !p.interface),
      enabled: t.enabled,
      sudoPassword,
    }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('targets.save'));
    if (!res) return;
    followResponse(screen, res, onChange, T('targets.saved_done', { name: t.name }));
    if (win.isConnected) load();
  };

  win.addEventListener('action', (e) => { if (e.detail?.action === 'cancel') win.close(true); });
  load();
  return win;
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

/**
 * Deleting a target takes the export out of the kernel; the zvol and its data
 * stay. Retype-gated, and the node may still park it for a second admin — the
 * blast radius is a client losing a disk mid-write.
 */
export function openTargetDeleteDialog(screen, target, onDone) {
  const lun = (target.luns || [])[0];
  const bodyHtml = `
    ${warningHtml('danger', T('targets.delete_warning', { name: target.name }))}
    <ul class="loss-list">
      <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('targets.delete_loss_export', { proto: protocolLabel(target.protocol) }))}</span></li>
      ${target.sessions ? `<li class="ll bad">${sprite('x')}<span>${escapeHtml(T('targets.delete_loss_sessions', { n: target.sessions }))}</span></li>` : ''}
      ${target.sessionsKnown !== true ? `<li class="ll bad">${sprite('alert')}<span>${escapeHtml(T('targets.delete_loss_sessions_unknown'))}</span></li>` : ''}
      <li class="ll good">${sprite('check')}<span>${escapeHtml(T('targets.delete_keep_volume', { source: lun ? lun.source : '—' }))}</span></li>
    </ul>`;
  return openRetypeDialog({
    title: T('targets.delete_title', { name: target.name }),
    icon: 'trash',
    name: target.name,
    bodyHtml,
    confirmLabel: T('targets.delete'),
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasTargetDeleteRequest', { targetId: target.targetId, confirmName: target.name, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('targets.delete_title', { name: target.name }));
      if (res === null) return false;
      followResponse(screen, res, onDone, T('targets.deleted_done', { name: target.name }));
      return true;
    },
  });
}
