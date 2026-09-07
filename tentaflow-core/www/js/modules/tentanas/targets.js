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
import { portalDrifted, bindableAddresses } from '/js/modules/tentanas/target-wizard.js';
import { primaryAddress } from '/js/modules/tentanas/target-wizard.js';

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
  const sourceNodeId = screen.nodeId;
  const isCurrent = () => host.isConnected && !screen.disposed && screen.nodeId === sourceNodeId;
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
    openTargetWizard(screen, { capabilities: state.capabilities, targets: state.targets, onDone: refresh, isCurrent });
  };
  // The pencil opens the WIZARD, which is what n12 says it does. The detail
  // window is one row-click away and carries the things the wizard does not
  // (the iSCSI allowlist, the rendered configfs plan), so both surfaces are
  // reachable in one click instead of the wizard being two.
  const openEdit = (target) => {
    if (!guardAdmin()) return;
    openTargetWizard(screen, { target, capabilities: state.capabilities, targets: state.targets, onDone: refresh, isCurrent });
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
        wrap.querySelector('[data-act="details"]')?.addEventListener('click', (e) => { e.stopPropagation(); screen.openTarget(t.targetId); });
        wrap.querySelector('[data-act="edit"]')?.addEventListener('click', (e) => { e.stopPropagation(); openEdit(t); });
        wrap.querySelector('[data-act="pause"]')?.addEventListener('click', (e) => { e.stopPropagation(); setTargetEnabled(screen, t, !t.enabled, refresh, isCurrent); });
        wrap.querySelector('[data-act="delete"]')?.addEventListener('click', (e) => { e.stopPropagation(); openTargetDeleteDialog(screen, t, refresh, isCurrent); });
        return wrap;
      };
      table.addEventListener('row-click', (e) => screen.openTarget(e.detail.row._target.targetId));
    }
    table.rows = visible().map(targetRow);
    // Nazwa z alertu zostaje rozwiązana do trwałego identyfikatora targetu.
    const wanted = screen.targetName;
    if (wanted) {
      screen.targetName = null;
      const row = state.targets.find((t) => t.name === wanted);
      if (row) {
        screen.openTarget(row.targetId);
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
export async function setTargetEnabled(screen, target, enabled, onDone, isCurrent) {
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
  }, { timeoutMs: ADMIN_TIMEOUT_MS }), title, isCurrent);
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
export const sessionLine = (s) => (s.client && s.user && s.user !== s.client
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
export function openTargetDetail(screen, targetId, { body, capabilities = null, siblings = [], onChange = null } = {}) {
  const win = document.createElement('section');
  const sourceNodeId = screen.nodeId;
  const isCurrent = () => win.isConnected && !screen.disposed && screen.nodeId === sourceNodeId;
  win.className = 'nas-target-detail';
  win.innerHTML = `<div class="nas-target-page-head"><tf-button variant="ghost" icon="arrow-left" data-act="back">${escapeHtml(T('targets.back_to_list'))}</tf-button></div>
    <div slot="body" class="stack"><div class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>`;
  body.replaceChildren(win);
  win.querySelector('[data-act="back"]').addEventListener('click', () => screen.openTarget(null));
  const state = { target: null, sessions: [], preview: '', initiatorsText: '', capabilities, siblings };

  const load = async () => {
    if (!isCurrent()) return false;
    const requestDraft = state.initiatorsText;
    const hasDraft = state.target && state.initiatorsText !== (state.target.initiators || []).join('\n');
    try {
      const [r, list] = await Promise.all([
        screen.nas('tentaNasTargetGetRequest', { targetId }),
        screen.nas('tentaNasTargetsListRequest', {}).catch(() => null),
      ]);
      if (!isCurrent()) return false;
      state.capabilities = list?.capabilities || null;
      state.siblings = list?.targets || siblings;
      state.target = r.target;
      state.sessions = r.sessions || [];
      state.preview = r.configPreview || '';
      if (!hasDraft && state.initiatorsText === requestDraft) state.initiatorsText = (r.target.initiators || []).join('\n');
    } catch (e) {
      if (isCurrent()) win.querySelector('[slot="body"]').innerHTML = `<div class="num-err">${escapeHtml(errMessage(e))}</div>`;
      return false;
    }
    if (isCurrent()) draw();
    return true;
  };

  // The same node-wide-host warning the wizard shows, on the other surface an
  // allowlist is edited from — and with the SAME sentence-picking rule.
  // Hard-coding `dhchap_hosts_shared` here advised "set the same key here" in
  // a window that has no key field, on targets that have no key: this passes
  // the target's own method to the one function that chooses.
  const sharedWarningHtml = (t) => {
    const shared = sharedHostWarning(
      state.siblings,
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
    const drifted = portalDrifted(t, state.capabilities);
    const portal = (t.portals || [])[0];
    const interfaces = state.capabilities?.interfaces;
    const owners = Array.isArray(interfaces) && portal
      ? [...new Set(interfaces.filter((i) => i.supported && i.address === portal.address).map((i) => i.name))]
      : null;
    const portalRows = portal ? [
      ['targets.portal_configured', `${portal.address}:${portal.port}`],
      ['targets.portal_expected', portal.interface || T('targets.all_interfaces')],
      ['targets.portal_current_addresses', Array.isArray(interfaces) ? bindableAddresses(state.capabilities, portal.interface).join(', ') || '—' : T('targets.portal_unknown')],
      ['targets.portal_actual', !portal.interface ? T('targets.all_interfaces') : owners ? owners.join(', ') || T('targets.portal_no_owner') : T('targets.portal_unknown')],
      ['targets.portal_transport', [...new Set(t.portals.map((p) => transportLabel(p.transport)))].join(' + ')],
      ['targets.portal_exposure', T('targets.portal_unknown')],
    ].map(([key, value]) => `<div class="sr"><span class="k">${escapeHtml(T(key))}</span><span class="v mono" data-testid="${key.slice('targets.'.length)}">${escapeHtml(value)}</span></div>`) : [];
    const lun = (t.luns || [])[0];
    const authRows = t.auth?.method && t.auth.method !== 'none' ? (t.protocol === 'nvmet' ? [
      [T('targets.auth_hash'), t.auth.dhchapHash || '—'],
      [T('targets.auth_dhgroup'), t.auth.dhchapDhgroup || '—'],
      [T('wizard_target.dhchap_key'), t.auth.secretSet ? '••••••••••••' : '—'],
      [T('wizard_target.dhchap_ctrl_key'), t.auth.mutualSecretSet ? '••••••••••••' : '—'],
    ] : [
      [T('wizard_target.auth_user'), t.auth.username || '—'],
      [T('wizard_target.auth_secret'), t.auth.secretSet ? '••••••••••••' : '—'],
      [T('wizard_target.auth_mutual_user'), t.auth.mutualUsername || '—'],
      [T('wizard_target.auth_mutual_secret'), t.auth.mutualSecretSet ? '••••••••••••' : '—'],
    ]).map(([label, value]) => `<div class="sr"><span class="k">${escapeHtml(label)}</span><span class="v mono">${escapeHtml(value)}</span></div>`) : [];
    const groups = (t.portGroups || []).map((g) => `<div class="sr"><span class="k">${escapeHtml(T('targets.port_group_row', { n: g.groupId }))}</span><span class="v">${escapeHtml(groupStateLabel(g.state))}${g.preferred ? ` · ${escapeHtml(T('targets.group_preferred'))}` : ''}</span></div>`).join('');
    win.innerHTML = `
      <div class="section-card-head nas-target-page-head"><div class="row"><tf-button variant="ghost" icon="arrow-left" data-act="back">${escapeHtml(T('targets.back_to_list'))}</tf-button><h2>${escapeHtml(t.name)}</h2></div></div>
      <div slot="body" class="stack">
        ${drifted ? `<div class="wizard-warning danger" data-testid="portal-drift-banner">${sprite('alert')}<div>
          <b>${escapeHtml(T('targets.portal_drift_title'))}</b>
          <p>${escapeHtml(T('targets.portal_drift_note'))}</p>
          ${t.stateDetail ? `<p>${escapeHtml(t.stateDetail)}</p>` : ''}
          <div class="row">
            ${screen.isAdmin ? `<tf-button variant="primary" icon="globe" data-act="repick-portal">${escapeHtml(T('targets.portal_repick'))}</tf-button>` : ''}
            <tf-button variant="secondary" icon="refresh" data-act="refresh">${escapeHtml(T('targets.portal_refresh'))}</tf-button>
          </div>
        </div></div>` : ''}
        <section class="nas-target-card">
        <div class="section-card-head"><h3 class="title">${sprite('target')} ${escapeHtml(t.name)}</h3><div class="row">
          ${protocolChipHtml(t.protocol)}
          ${stateChip(t) || `<tf-chip size="sm" status="ok" dot label="${escapeAttr(T('targets.state_active'))}"></tf-chip>`}
        </div></div>
        ${t.stateDetail && !drifted ? `<p class="text-3">${escapeHtml(t.stateDetail)}</p>` : ''}
        <div class="nas-target-grid"><div class="stat-rows">
          <div class="sr"><span class="k">${escapeHtml(T('targets.wwn'))}</span><span class="v mono">${escapeHtml(t.wwn)}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('targets.lun'))}</span><span class="v mono">${lun ? `${escapeHtml(lun.source)} · ${escapeHtml(fmtBytes(lun.sizeBytes))}${lun.thin ? ' · thin' : ''}` : '—'}</span></div>
        </div><div class="stat-rows">
          <div class="sr"><span class="k">${escapeHtml(T('targets.sessions_title'))}</span><span class="v" data-testid="target-sessions-count">${escapeHtml(sessionsCountLabel(t))}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('targets.created'))}</span><span class="v">${escapeHtml(t.createdAt ? fmtAgo(t.createdAt) : '—')}</span></div>
        </div></div></section>
        <section class="nas-target-card" data-testid="target-portal-card">
        <div class="section-card-head"><h3 class="title">${sprite('globe')} ${escapeHtml(T('targets.portal_section'))}</h3>
          ${!drifted ? `<tf-button size="sm" variant="secondary" icon="refresh" data-act="refresh">${escapeHtml(T('targets.portal_refresh'))}</tf-button>` : ''}
        </div>
        <div class="nas-target-grid"><div class="stat-rows">${portalRows.slice(0, 3).join('')}</div><div class="stat-rows">${portalRows.slice(3).join('')}</div></div>
        <div class="section-card-head"><h3 class="title">${sprite('layers')} ${escapeHtml(T('targets.port_groups'))}</h3></div>
        <div class="stat-rows" id="nas-td-groups">${groups}</div>
        ${drifted ? `<div class="section-card-head"><h3 class="title">${sprite('globe')} ${escapeHtml(T('targets.portal_available'))}</h3></div>
          <p class="muted">${escapeHtml(T('targets.portal_available_note'))}</p><tf-table id="nas-td-interfaces">
            <tf-column key="address" label="${escapeAttr(T('targets.portal_address'))}"></tf-column>
            <tf-column key="interface" label="${escapeAttr(T('wizard_target.portal_label'))}"></tf-column>
            <tf-column key="network" label="${escapeAttr(T('targets.portal_network'))}"></tf-column>
          </tf-table>` : ''}
        </section>
        <section class="nas-target-card">
        <div class="section-card-head"><h3 class="title">${sprite('users')} ${escapeHtml(T('targets.sessions_title'))}</h3> <tf-chip size="sm" status="neutral" label="${escapeAttr(sessionsCountLabel(t))}"></tf-chip></div>
        ${state.sessions.length
          ? `<tf-table id="nas-td-sessions"><tf-column key="client" label="${escapeAttr(T('targets.session_identity'))}" fill></tf-column><tf-column key="identity" label="IQN / NQN" renderer="html" fill></tf-column></tf-table>`
          : t.sessionsKnown !== true ? `<tf-empty-state icon="users" title="${escapeAttr(T('targets.sessions_unmeasured'))}" message="${escapeAttr(sessionsEmptyText(t))}"></tf-empty-state>` : `<div class="muted">${escapeHtml(sessionsEmptyText(t))}</div>`}
        </section>
        <section class="nas-target-card">
        <div class="section-card-head"><h3 class="title">${sprite('shield')} ${escapeHtml(T('targets.initiators'))}</h3></div>
        <tf-table id="nas-td-hosts" empty-message="${escapeAttr(T('targets.no_initiators'))}"><tf-column key="identity" label="IQN / NQN" fill></tf-column><tf-column key="auth" label="${escapeAttr(T('targets.col_auth'))}" renderer="html"></tf-column><tf-column key="shared" label="${escapeAttr(T('targets.host_shared'))}"></tf-column></tf-table>
        ${screen.isAdmin ? `<details><summary>${escapeHtml(T('targets.edit_initiators'))}</summary>
          <tf-input id="nas-td-initiators" multiline rows="3" spellcheck="false" hint="${escapeAttr(T('targets.initiators_hint'))}" value="${escapeAttr(state.initiatorsText)}"></tf-input>
          </details><p class="muted" data-testid="initiators-draft-hint">${escapeHtml(T('targets.initiators_draft'))}</p>` : ''}
        <div id="nas-td-shared">${sharedWarningHtml(t)}</div>
        ${warningHtml('info', T('targets.allowlist_note'))}
        </section>
        <section class="nas-target-card">
        <div class="section-card-head"><h3 class="title">${sprite('lock')} ${escapeHtml(T('targets.col_auth'))}</h3>${authChipHtml(t.auth)}</div>
        <div class="stat-rows"><div class="sr"><span class="k">${escapeHtml(T('targets.col_auth'))}</span><span class="v">${escapeHtml(authLabel(t.auth?.method))}${t.auth?.username ? ` · <span class="mono">${escapeHtml(t.auth.username)}</span>` : ''}</span></div></div>
        <div class="nas-target-grid"><div class="stat-rows">${authRows.slice(0, 2).join('')}</div><div class="stat-rows">${authRows.slice(2).join('')}</div></div>
        <p class="muted">${escapeHtml(T('targets.auth_stored_note'))}</p>
        </section>
        ${warningHtml('danger', T('targets.raw_disk_note'))}
        <section class="nas-target-card">
        <div class="section-card-head"><h3 class="title">${sprite('terminal')} ${escapeHtml(T('targets.config_preview'))}</h3></div><p class="muted">${escapeHtml(T('targets.config_preview_hint'))}</p>
        <pre class="cmd" id="nas-td-preview">${escapeHtml(state.preview)}</pre>
        </section>
        ${screen.isAdmin ? `<section class="nas-target-card nas-target-danger">
          <h3 class="title">${escapeHtml(T('targets.delete_title', { name: t.name }))}</h3>
          <p>${escapeHtml(T('targets.delete_keep_volume', { source: lun?.source || '—' }))}</p>
          <div><tf-button variant="danger" icon="trash" data-act="delete">${escapeHtml(T('targets.delete'))}</tf-button></div>
        </section>` : ''}
      </div>
      <div slot="footer">
        ${screen.isAdmin ? `<tf-button variant="ghost" icon="${t.enabled ? 'pause' : 'play'}" data-act="pause">${escapeHtml(t.enabled ? T('targets.pause') : T('targets.resume'))}</tf-button>` : ''}
        <span class="spacer"></span>
        ${screen.isAdmin ? `<tf-button variant="secondary" icon="edit" data-act="edit">${escapeHtml(T('targets.edit'))}</tf-button>
        <tf-button variant="primary" icon="check" data-act="save">${escapeHtml(T('targets.save'))}</tf-button>` : ''}
      </div>`;
    const openPortalSelection = (portalSelection = null) => {
      openTargetWizard(screen, { target: t, capabilities: state.capabilities, targets: state.siblings, onDone: () => { onChange?.(); load(); }, selectPortal: true, portalSelection, isCurrent });
    };
    const interfaceTable = win.querySelector('#nas-td-interfaces');
    if (interfaceTable) {
      if (screen.isAdmin) interfaceTable.rowActions = (row) => {
        const button = document.createElement('tf-button');
        button.setAttribute('size', 'sm');
        button.setAttribute('variant', 'secondary');
        button.setAttribute('data-act', 'pick-interface');
        button.textContent = T('targets.portal_pick_interface');
        if (!row._supported || row.address !== primaryAddress(state.capabilities, row.interface)) {
          button.setAttribute('disabled', '');
          button.setAttribute('title', T('targets.portal_available_note'));
        }
        button.addEventListener('click', () => openPortalSelection(row.interface));
        return button;
      };
      interfaceTable.rows = interfaces.map((i) => ({ address: i.address, interface: i.name, network: T(i.shared ? 'targets.portal_network_shared' : 'targets.portal_network_storage'), _supported: i.supported }));
    }
    const sessionTable = win.querySelector('#nas-td-sessions');
    if (sessionTable) sessionTable.rows = state.sessions.map((s) => ({ client: s.client || '—', identity: sessionLine({ user: s.user }) }));
    const hostsTable = win.querySelector('#nas-td-hosts');
    const updateHosts = () => {
      hostsTable.rows = parseInitiators(state.initiatorsText).map((identity) => ({
        identity,
        auth: authChipHtml(t.auth),
        shared: state.capabilities ? state.siblings.filter((other) => other.targetId !== t.targetId && other.protocol === t.protocol && (other.initiators || []).includes(identity)).map((other) => other.name).join(', ') || '—' : T('targets.portal_unknown'),
      }));
      win.querySelector('#nas-td-shared').innerHTML = sharedWarningHtml(t);
    };
    if (screen.isAdmin) hostsTable.rowActions = (row) => {
      const button = document.createElement('tf-button');
      button.setAttribute('variant', 'ghost');
      button.setAttribute('tone', 'critical');
      button.setAttribute('size', 'sm');
      button.setAttribute('icon', 'trash');
      button.textContent = T('targets.remove_initiator');
      button.addEventListener('click', () => {
        state.initiatorsText = parseInitiators(state.initiatorsText).filter((host) => host !== row.identity).join('\n');
        win.querySelector('#nas-td-initiators').value = state.initiatorsText;
        updateHosts();
      });
      return button;
    };
    updateHosts();
    win.querySelector('#nas-td-initiators')?.addEventListener('input', (e) => { state.initiatorsText = e.target.value; updateHosts(); });
    win.querySelector('[data-act="back"]').addEventListener('click', () => screen.openTarget(null));
    win.querySelector('[data-act="save"]')?.addEventListener('click', () => saveAllowlist());
    win.querySelector('[data-act="edit"]')?.addEventListener('click', () => {
      // The node's real target list, not `[t]`: `sharedHostTargets` excludes
      // the target being edited, so a one-element list always filtered to
      // empty and this path — the ordinary way to edit an existing target —
      // showed no shared-host warning at all.
      openTargetWizard(screen, { target: t, capabilities: state.capabilities, targets: state.siblings, onDone: () => { onChange?.(); load(); }, isCurrent });
    });
    win.querySelector('[data-act="repick-portal"]')?.addEventListener('click', () => openPortalSelection());
    win.querySelector('[data-act="refresh"]')?.addEventListener('click', () => load());
    win.querySelector('[data-act="delete"]')?.addEventListener('click', () => {
      openTargetDeleteDialog(screen, t, () => { onChange?.(); if (isCurrent()) screen.openTarget(null); }, isCurrent);
    });
    win.querySelector('[data-act="pause"]')?.addEventListener('click', async () => {
      await setTargetEnabled(screen, t, !t.enabled, onChange, isCurrent);
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
    }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('targets.save'), isCurrent);
    if (!res) return;
    followResponse(screen, res, onChange, T('targets.saved_done', { name: t.name }));
    state.target.initiators = parseInitiators(state.initiatorsText);
    if (win.isConnected) load();
  };

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
export function openTargetDeleteDialog(screen, target, onDone, isCurrent) {
  const lun = (target.luns || [])[0];
  const bodyHtml = `
    ${warningHtml('danger', T('targets.delete_warning', { name: target.name }))}
    <ul class="loss-list">
      <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('targets.delete_loss_export', { proto: protocolLabel(target.protocol) }))}</span></li>
      <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('targets.delete_loss_record'))} <span class="mono">${escapeHtml(target.wwn)}</span></span></li>
      ${(target.initiators || []).length ? `<li class="ll bad">${sprite('x')}<span>${escapeHtml(T('targets.delete_loss_allowlist', { n: target.initiators.length }))}</span></li>` : ''}
      <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('targets.delete_loss_auth'))}</span></li>
      ${target.sessions ? `<li class="ll bad">${sprite('x')}<span>${escapeHtml(T('targets.delete_loss_sessions', { n: target.sessions }))}</span></li>` : ''}
      ${target.sessionsKnown !== true ? `<li class="ll bad">${sprite('alert')}<span>${escapeHtml(T('targets.delete_loss_sessions_unknown'))}</span></li>` : ''}
      <li class="ll good">${sprite('check')}<span>${escapeHtml(T('targets.delete_keep_volume', { source: lun ? lun.source : '—' }))}</span></li>
    </ul>
    <div class="explain-box mt-md"><b>${escapeHtml(T('targets.delete_keep_volume', { source: lun ? lun.source : '—' }))}</b> ${escapeHtml(T('targets.delete_keep_snapshots'))}</div>`;
  const win = openRetypeDialog({
    title: T('targets.delete_title', { name: target.name }),
    icon: 'trash',
    name: target.name,
    bodyHtml,
    confirmLabel: T('targets.delete'),
    onConfirm: async () => {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasTargetDeleteRequest', { targetId: target.targetId, confirmName: target.name, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('targets.delete_title', { name: target.name }), isCurrent);
      if (res === null) return false;
      followResponse(screen, res, onDone, T('targets.deleted_done', { name: target.name }));
      return true;
    },
  });
  win.classList.add('nas-target-delete');
  return win;
}
