// ===== File: modules/tentanas/share-wizard.js — the "Nowy share" wizard (n13): type and source, access (SMB toggles + grants / NFS networks), fleet mount and summary; edit mode reuses the last two steps =====
//
// Same window, header, progress rail and footer as the addon install wizard
// (the way pool-wizard.js does it). Creating and editing send one request
// each through `withSudo`; the job answer opens the job log and the list
// refreshes when it finishes. Edit mode keeps the name, protocol and source
// read-only — the services identify a share by them, so changing any of the
// three is a delete-and-create, not an update.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, ADMIN_TIMEOUT_MS, errMessage, jobKindLabel, transportLabel } from '/js/modules/tentanas/format.js';
import { openShareUsersDialog } from '/js/modules/tentanas/share-users.js';
import { pathCrumbsHtml, wirePathCrumbs } from '/js/modules/tentanas/dialogs.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-choice-card.js';
import '/js/components/tf-table.js';
import '/js/components/tf-checkbox.js';

// Share names become the SMB share / NFS export name and the mountpoint on
// every node — letters, digits, `_` and `-`, up to 64 characters.
const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/;
export const shareNameValid = (name) => NAME_RE.test(name);

// One CIDR or host per line; blanks and duplicates fall away.
export const parseNetworks = (text) => [...new Set(String(text || '').split(/[\n,;]+/).map((x) => x.trim()).filter(Boolean))];

// The audit starts on refused operations only: that is the setting an admin
// switching it on almost always wants, and it is the one that cannot drown the
// log in a line per read (§5.10).
const defaultSmb = () => ({ guests: false, previousVersions: true, recycleBin: true, timeMachine: false, smbDirect: false, audit: false, auditGroups: ['writes'], auditSuccess: false, auditFailure: true, users: [] });
const defaultNfs = () => ({ networks: [], readOnly: false, rootSquash: true, asyncWrites: false, rdma: false, audit: false });

/// The operation groups the node audits, in the order `access_log.rs` lists
/// them. Ids only: the labels are i18n keys, and the operations each group
/// expands to belong to the node, not to the browser.
export const AUDIT_GROUPS = ['sessions', 'reads', 'writes', 'permissions'];

/**
 * A row of the node's environment probe, or null when the tab has not loaded
 * one yet. The wizard offers a transport from exactly the row the backend
 * re-checks on save, so the two can never disagree.
 */
export function featureRow(environment, id) {
  return (environment?.features || []).find((f) => f.id === id) || null;
}
export const rdmaFeature = (environment) => featureRow(environment, 'rdma');
export const rdmaAvailable = (environment) => rdmaFeature(environment)?.status === 'ok';

/**
 * The ksmbd row (§5.4b). It answers more than "are the tools installed": the
 * exposure guard lives in it too, so a node whose only RDMA interface also
 * carries the default gateway reads as unavailable here and the option is
 * never offered — which is what "the wizard does not give that option" means.
 */
export const ksmbdFeature = (environment) => featureRow(environment, 'ksmbd');
export const smbDirectAvailable = (environment) => ksmbdFeature(environment)?.status === 'ok';

/**
 * Per-node outcome of the fleet mount, derived from the fleet list: the
 * selected node is the source, a node without a NAS instance is n/a, a node
 * whose privilege channel is unarmed mounts once it is armed, any other node
 * mounts on its next reconcile.
 */
export function fleetPlan(nodes, currentNodeId) {
  return (nodes || []).map((n) => {
    let outcome;
    if (n.nodeId === currentNodeId) outcome = 'source';
    else if (n.instanceStatus !== 'ready') outcome = 'unsupported';
    else if ((n.elevationMode || 'unarmed') === 'unarmed') outcome = 'after_arm';
    else outcome = 'will_mount';
    return { nodeId: n.nodeId, nodeName: n.nodeName, outcome };
  });
}

const OUTCOME_CLASS = { source: 'num-ok', will_mount: 'num-ok', after_arm: 'num-warn', unsupported: 'text-3' };

export function openShareWizard(screen, { share = null, users = [], mountRoot = '/mnt/tentanas', onDone = null } = {}) {
  if (screen.openWindow) { screen.openWindow.remove(); screen.openWindow = null; }
  const node = screen.currentNode();
  const editing = Boolean(share);
  const state = {
    step: editing ? 1 : 0,
    protocol: share?.protocol || 'smb',
    name: share?.name || '',
    sourcePath: share?.sourcePath || '',
    dataset: share?.dataset || null,
    smb: share?.smb ? { ...defaultSmb(), ...share.smb, users: (share.smb.users || []).map((u) => ({ user: u.user, mode: u.mode === 'ro' ? 'ro' : 'rw' })) } : defaultSmb(),
    nfs: share?.nfs ? { ...defaultNfs(), ...share.nfs, networks: [...(share.nfs.networks || [])] } : defaultNfs(),
    networksText: (share?.nfs?.networks || []).join('\n'),
    fleetMount: share ? Boolean(share.fleetMount) : true,
    enabled: share ? Boolean(share.enabled) : true,
    users: users.slice(),
    grantPick: '',
    busy: false,
  };
  const steps = [T('wizard_share.step_type'), T('wizard_share.step_access'), T('wizard_share.step_fleet')];

  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', editing ? T('wizard_share.title_edit', { name: share.name }) : T('wizard_share.title'));
  win.setAttribute('icon', 'share');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '820');
  win.setAttribute('min-width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  screen.openWindow = win;

  const header = () => `
    <div class="install-header">
      <div class="big-ico">${sprite('share')}</div>
      <div class="install-header-meta">
        <h1>${escapeHtml(editing ? T('wizard_share.heading_edit', { name: share.name }) : T('wizard_share.heading'))} <span class="version">${escapeHtml(T('wizard.node_tag', { node: node.nodeName }))}</span></h1>
        <div class="sub">${escapeHtml(T('wizard_share.sub'))}</div>
      </div>
    </div>
    <div class="install-progress">${steps.map((s, i) => `<div class="install-step ${i === state.step ? 'active' : i < state.step ? 'done' : ''}"><span class="num">${i < state.step ? sprite('check') : i + 1}</span><span class="label">${escapeHtml(s)}</span></div>`).join('')}</div>`;

  // Step 1 — protocol, name and source. Read-only when editing.
  const stepType = () => `
    <h2 class="wizard-section-title">${escapeHtml(T('wizard_share.type_title'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(T('wizard_share.type_sub'))}</p>
    <tf-choice-group id="nas-sw-protocol" value="${escapeAttr(state.protocol)}" columns="2">
      <tf-choice-card value="smb" icon="share" heading="SMB" description="${escapeAttr(T('wizard_share.smb_desc'))}" ${editing ? 'disabled' : ''}></tf-choice-card>
      <tf-choice-card value="nfs" icon="folder" heading="NFS" description="${escapeAttr(T('wizard_share.nfs_desc'))}" ${editing ? 'disabled' : ''}></tf-choice-card>
    </tf-choice-group>
    <div class="form-grid-2 mt-md">
      <tf-input id="nas-sw-name" label="${escapeAttr(T('wizard_share.name_label'))}" placeholder="dokumenty" autocomplete="off" spellcheck="false" value="${escapeAttr(state.name)}" hint="${escapeAttr(T('wizard_share.name_hint'))}" ${editing ? 'readonly' : ''}></tf-input>
      <div class="stack" style="gap:6px">
        <tf-input id="nas-sw-source" label="${escapeAttr(T('wizard_share.source_label'))}" placeholder="/tank/dokumenty" autocomplete="off" spellcheck="false" value="${escapeAttr(state.sourcePath)}" hint="${escapeAttr(T('wizard_share.source_hint'))}" ${editing ? 'readonly' : ''}></tf-input>
        ${editing ? '' : `<div class="row"><tf-button size="sm" variant="secondary" icon="folder" data-act="browse">${escapeHtml(T('wizard_share.browse'))}</tf-button>${state.dataset ? `<tf-chip size="sm" status="info" label="${escapeAttr(state.dataset)}"></tf-chip>` : ''}</div>`}
      </div>
    </div>`;

  const toggleCard = (id, label, sub, checked, disabled = false) => `
    <div class="toggle-card">
      <div class="tc-text"><span>${escapeHtml(label)}</span><span class="tc-sub">${escapeHtml(sub)}</span></div>
      <tf-toggle id="${id}" ${checked ? 'checked' : ''} ${disabled ? 'disabled' : ''}></tf-toggle>
    </div>`;

  // Step 2 — access. SMB: the four toggles and the grants; NFS: networks
  // and the export options.
  const stepAccess = () => (state.protocol === 'smb' ? stepAccessSmb() : stepAccessNfs());

  // "SMB Direct (RDMA)" (§5.4b). Samba has no SMB3-over-RDMA, so a share with
  // this option is served a second time by ksmbd on the node's RDMA
  // interfaces — and that path carries none of the four options above. The
  // losses are listed before the toggle is even on, because they are the
  // reason the option is a decision and not a speed setting.
  const stepAccessSmbDirect = () => {
    const feature = ksmbdFeature(screen.environment);
    const ok = smbDirectAvailable(screen.environment);
    const card = toggleCard(
      'nas-sw-smbdirect',
      T('wizard_share.smb_direct'),
      ok ? T('wizard_share.smb_direct_sub') : T('wizard_share.smb_direct_unavailable'),
      state.smb.smbDirect,
      !ok,
    );
    const losses = ['audit', 'previous_versions', 'recycle_bin', 'time_machine', 'zfs_acl', 'multichannel']
      .map((k) => `<li class="ll bad">${sprite('x')}<span>${escapeHtml(T('wizard_share.smb_direct_loss_' + k))}</span></li>`)
      .join('');
    if (ok && state.smb.smbDirect) {
      return `${card}
        <div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(T('wizard_share.smb_direct_note'))}</div></div>
        <ul class="loss-list">${losses}</ul>`;
    }
    if (!ok && feature?.detail) {
      return `${card}<div class="muted">${escapeHtml(feature.detail)}</div>`;
    }
    return card;
  };

  // "Audytuj dostęp" (§5.10). The groups and the two results are what the
  // share section turns into `full_audit:success`/`failure`, so the wizard
  // shows exactly the choice the node accepts — and says out loud that the
  // SMB Direct path of the same share is NOT audited (§5.4b).
  const stepAccessAudit = () => {
    const card = toggleCard(
      'nas-sw-audit',
      T('wizard_share.audit'),
      T('wizard_share.audit_sub'),
      state.smb.audit,
    );
    if (!state.smb.audit) return card;
    const groups = AUDIT_GROUPS.map((id) => `<tf-checkbox data-audit-group="${escapeAttr(id)}" label="${escapeAttr(T('wizard_share.audit_group_' + id))}" ${state.smb.auditGroups.includes(id) ? 'checked' : ''}></tf-checkbox>`).join('');
    const results = [['success', state.smb.auditSuccess], ['failure', state.smb.auditFailure]].map(([id, on]) => `<tf-checkbox data-audit-result="${escapeAttr(id)}" label="${escapeAttr(T('wizard_share.audit_result_' + id))}" ${on ? 'checked' : ''}></tf-checkbox>`).join('');
    const empty = !state.smb.auditGroups.length || (!state.smb.auditSuccess && !state.smb.auditFailure);
    return `${card}
      <div class="field">
        <label>${escapeHtml(T('wizard_share.audit_groups'))}</label>
        <div class="row" id="nas-sw-audit-groups">${groups}</div>
      </div>
      <div class="field">
        <label>${escapeHtml(T('wizard_share.audit_results'))}</label>
        <div class="row" id="nas-sw-audit-results">${results}</div>
      </div>
      ${empty ? `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_share.audit_empty'))}</div></div>` : ''}
      ${state.smb.smbDirect ? `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_share.audit_smb_direct'))}</div></div>` : ''}
      <div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(T('wizard_share.audit_note'))}</div></div>`;
  };

  const stepAccessSmb = () => {
    const granted = new Set(state.smb.users.map((u) => u.user));
    const free = state.users.filter((u) => !granted.has(u.name));
    return `
      <h2 class="wizard-section-title">${escapeHtml(T('wizard_share.access_title_smb'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('wizard_share.access_sub'))}</p>
      <div class="stack">
        ${toggleCard('nas-sw-guests', T('wizard_share.guests'), T('wizard_share.guests_sub'), state.smb.guests)}
        ${toggleCard('nas-sw-prev', T('wizard_share.previous_versions'), T('wizard_share.previous_versions_sub'), state.smb.previousVersions)}
        ${toggleCard('nas-sw-recycle', T('wizard_share.recycle_bin'), T('wizard_share.recycle_bin_sub'), state.smb.recycleBin)}
        ${toggleCard('nas-sw-tm', T('wizard_share.time_machine'), T('wizard_share.time_machine_sub'), state.smb.timeMachine)}
        ${stepAccessSmbDirect()}
        ${stepAccessAudit()}
        <div class="field">
          <div class="row"><b>${escapeHtml(T('wizard_share.users_title'))}</b><span class="spacer" style="flex:1"></span><tf-button size="sm" variant="ghost" icon="users" data-act="manage-users">${escapeHtml(T('wizard_share.manage_users'))}</tf-button></div>
          <div class="stat-rows" id="nas-sw-grants">
            ${state.smb.users.length ? state.smb.users.map((u) => `
            <div class="sr" data-user="${escapeAttr(u.user)}">
              <span class="k mono">${escapeHtml(u.user)}</span>
              <span class="v">
                <tf-select data-grant-mode="${escapeAttr(u.user)}" style="width:140px"></tf-select>
                <tf-button size="sm" variant="ghost" icon="x" data-grant-remove="${escapeAttr(u.user)}" title="${escapeAttr(T('wizard_share.grant_remove'))}"></tf-button>
              </span>
            </div>`).join('') : `<div class="muted">${escapeHtml(state.smb.guests ? T('wizard_share.no_grants_guests') : T('wizard_share.no_grants'))}</div>`}
          </div>
          <div class="row mt-sm">
            <tf-select id="nas-sw-grant-pick" style="width:240px" ${free.length ? '' : 'disabled'}></tf-select>
            <tf-button size="sm" variant="secondary" icon="plus" data-act="grant-add" ${free.length ? '' : 'disabled'}>${escapeHtml(T('wizard_share.grant_add'))}</tf-button>
            ${state.users.length ? '' : `<span class="muted">${escapeHtml(T('wizard_share.no_users_yet'))}</span>`}
          </div>
        </div>
        ${!state.smb.guests && !state.smb.users.length ? `<div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(T('wizard_share.nobody_warning'))}</div></div>` : ''}
      </div>`;
  };

  // "Transport: TCP / TCP + RDMA" (§5.5a). The toggle is only live when this
  // node's RDMA probe says ok; a node without a usable device shows it
  // disabled with the probe's own reason instead of hiding the option — and
  // keeps whatever the share already stored, so a link that went down does
  // not quietly rewrite the admin's choice on the next unrelated edit.
  const stepAccessNfsTransport = () => {
    const feature = rdmaFeature(screen.environment);
    const ok = rdmaAvailable(screen.environment);
    const card = toggleCard(
      'nas-sw-rdma',
      T('wizard_share.transport'),
      ok ? T('wizard_share.transport_sub') : T('wizard_share.transport_unavailable'),
      state.nfs.rdma,
      !ok,
    );
    if (ok && state.nfs.rdma) {
      return `${card}<div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(T('wizard_share.transport_note'))}</div></div>`;
    }
    if (!ok && feature?.detail) {
      return `${card}<div class="muted">${escapeHtml(feature.detail)}</div>`;
    }
    return card;
  };

  const stepAccessNfs = () => `
    <h2 class="wizard-section-title">${escapeHtml(T('wizard_share.access_title_nfs'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(T('wizard_share.access_sub'))}</p>
    <div class="stack">
      <tf-input id="nas-sw-networks" multiline rows="3" label="${escapeAttr(T('wizard_share.networks'))}" placeholder="10.10.0.0/24" spellcheck="false" hint="${escapeAttr(T('wizard_share.networks_hint'))}" value="${escapeAttr(state.networksText)}"></tf-input>
      <div class="row" id="nas-sw-network-chips">${state.nfs.networks.map((n) => `<tf-chip size="sm" status="neutral" label="${escapeAttr(n)}"></tf-chip>`).join('')}</div>
      ${toggleCard('nas-sw-ro', T('wizard_share.read_only'), T('wizard_share.read_only_sub'), state.nfs.readOnly)}
      ${toggleCard('nas-sw-squash', T('wizard_share.root_squash'), T('wizard_share.root_squash_sub'), state.nfs.rootSquash)}
      ${toggleCard('nas-sw-async', T('wizard_share.async_writes'), T('wizard_share.async_writes_sub'), state.nfs.asyncWrites)}
      ${stepAccessNfsTransport()}
      ${toggleCard('nas-sw-nfs-audit', T('wizard_share.audit'), T('wizard_share.audit_nfs_sub'), state.nfs.audit)}
      ${state.nfs.audit ? `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_share.audit_nfs_warning'))}</div></div>` : ''}
      ${state.nfs.asyncWrites ? `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_share.async_warning'))}</div></div>` : ''}
      ${state.nfs.networks.length ? '' : `<div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(T('wizard_share.networks_required'))}</div></div>`}
    </div>`;

  // Step 3 — fleet mount and the summary.
  const stepFleet = () => {
    const plan = fleetPlan(screen.nodes, node.nodeId);
    const path = `${mountRoot}/${state.name}`;
    return `
      <h2 class="wizard-section-title">${escapeHtml(T('wizard_share.fleet_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('wizard_share.fleet_sub'))}</p>
      <div class="stack">
        <div class="toggle-card">
          <div class="tc-text"><span>${escapeHtml(T('wizard_share.fleet_mount'))}</span><span class="tc-sub">${escapeHtml(T('wizard_share.fleet_mount_sub', { path }))}</span></div>
          <tf-toggle id="nas-sw-fleet" ${state.fleetMount ? 'checked' : ''}></tf-toggle>
        </div>
        ${state.fleetMount ? `<div class="stat-rows" id="nas-sw-fleet-plan">${plan.map((p) => `
          <div class="sr" data-node="${escapeAttr(p.nodeId)}"><span class="k"><span class="mono fw-700">${escapeHtml(p.nodeName)}</span></span><span class="v ${OUTCOME_CLASS[p.outcome]}">${escapeHtml(T('wizard_share.outcome_' + p.outcome))}</span></div>`).join('')}</div>`
          : `<div class="muted">${escapeHtml(T('wizard_share.fleet_off_note'))}</div>`}
        ${editing ? toggleCard('nas-sw-enabled', T('wizard_share.enabled'), T('wizard_share.enabled_sub'), state.enabled) : ''}
        <div class="stat-rows mt-md">
          <div class="sr"><span class="k">${escapeHtml(T('wizard_share.sum_share'))}</span><span class="v"><span class="mono">${escapeHtml(state.name)}</span> · ${escapeHtml(state.protocol.toUpperCase())}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('wizard_share.sum_source'))}</span><span class="v mono">${escapeHtml(state.sourcePath)}${state.dataset ? ` <tf-chip size="sm" status="info" label="${escapeAttr(state.dataset)}"></tf-chip>` : ''}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('wizard_share.sum_access'))}</span><span class="v">${escapeHtml(accessSummary())}</span></div>
        </div>
      </div>`;
  };

  const onOff = (v) => T(v ? 'shares.on' : 'shares.off');
  const accessSummary = () => {
    if (state.protocol === 'smb') {
      const s = state.smb;
      return [
        T('wizard_share.sum_users', { n: s.users.length }),
        `${T('wizard_share.guests').toLowerCase()}: ${onOff(s.guests)}`,
        `${T('wizard_share.previous_versions').toLowerCase()}: ${onOff(s.previousVersions)}`,
        `${T('wizard_share.recycle_bin').toLowerCase()}: ${onOff(s.recycleBin)}`,
        s.timeMachine ? `${T('wizard_share.time_machine')}: ${onOff(true)}` : '',
        s.smbDirect ? T('shares.smb_direct_chip') : '',
        s.audit ? `${T('wizard_share.audit').toLowerCase()}: ${onOff(true)}` : '',
      ].filter(Boolean).join(' · ');
    }
    const n = state.nfs;
    return [
      T('wizard_share.sum_networks', { n: n.networks.length }),
      n.readOnly ? T('wizard_share.read_only_short') : T('wizard_share.read_write_short'),
      `root_squash: ${onOff(n.rootSquash)}`,
      n.asyncWrites ? 'async' : 'sync',
      transportLabel(n.rdma),
      n.audit ? `${T('wizard_share.audit').toLowerCase()}: ${onOff(true)}` : '',
    ].filter(Boolean).join(' · ');
  };

  const canProceed = () => {
    if (state.busy) return false;
    if (state.step === 0) return shareNameValid(state.name) && state.sourcePath.startsWith('/');
    if (state.step === 1) {
      // The node refuses an audit with no group or no result (§5.10); the
      // wizard stops before the request rather than after the error.
      if (state.protocol === 'smb') {
        return !state.smb.audit
          || (state.smb.auditGroups.length > 0 && (state.smb.auditSuccess || state.smb.auditFailure));
      }
      return state.nfs.networks.length > 0;
    }
    return true;
  };

  const footer = () => {
    const last = state.step === 2;
    const first = state.step === (editing ? 1 : 0);
    const next = last
      ? `<tf-button variant="primary" icon="${editing ? 'check' : 'share'}" data-wizard-next ${canProceed() ? '' : 'disabled'}>${escapeHtml(editing ? T('wizard_share.save_button') : T('wizard_share.create_button'))}</tf-button>`
      : `<tf-button variant="primary" icon="chevron-right" data-wizard-next ${canProceed() ? '' : 'disabled'}>${escapeHtml(I18n.t('common.next'))}</tf-button>`;
    return `
      <tf-button variant="ghost" data-wizard-cancel ${state.busy ? 'disabled' : ''}>${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="ghost" icon="chevron-left" data-wizard-back ${first || state.busy ? 'disabled' : ''}>${escapeHtml(I18n.t('common.back'))}</tf-button>
      <span class="spacer"></span>
      ${next}`;
  };

  const syncNext = () => {
    const btn = win.querySelector('[data-wizard-next]');
    if (!btn) return;
    if (canProceed()) btn.removeAttribute('disabled');
    else btn.setAttribute('disabled', '');
  };

  const draw = () => {
    win.innerHTML = `
      <div slot="body">
        ${header()}
        <div class="install-step-body">${[stepType, stepAccess, stepFleet][state.step]()}</div>
      </div>
      <div slot="footer">${footer()}</div>`;
    wire();
  };

  const toggleValue = (e) => Boolean(e.detail?.checked ?? e.target.checked);
  const onToggle = (id, apply, redraw = false) => {
    win.querySelector('#' + id)?.addEventListener('change', (e) => { apply(toggleValue(e)); if (redraw) draw(); else syncNext(); });
  };

  const wire = () => {
    win.querySelector('#nas-sw-protocol')?.addEventListener('change', (e) => { state.protocol = e.detail.value; syncNext(); });
    const name = win.querySelector('#nas-sw-name');
    if (name) {
      const onName = () => {
        state.name = name.value.trim();
        if (state.name && !shareNameValid(state.name)) name.setAttribute('error', T('wizard_share.name_invalid'));
        else name.removeAttribute('error');
        syncNext();
      };
      name.addEventListener('input', onName);
      name.addEventListener('change', onName);
    }
    const source = win.querySelector('#nas-sw-source');
    if (source) {
      const onSource = () => { state.sourcePath = source.value.trim(); state.dataset = null; syncNext(); };
      source.addEventListener('input', onSource);
      source.addEventListener('change', onSource);
    }
    win.querySelector('[data-act="browse"]')?.addEventListener('click', () => openShareBrowseDialog(screen, {
      path: state.sourcePath,
      onPick: (entry) => { state.sourcePath = entry.path; state.dataset = entry.dataset || null; if (!state.name && entry.name && shareNameValid(entry.name)) state.name = entry.name; draw(); },
    }));

    // SMB access
    onToggle('nas-sw-guests', (v) => { state.smb.guests = v; }, true);
    onToggle('nas-sw-prev', (v) => { state.smb.previousVersions = v; });
    onToggle('nas-sw-recycle', (v) => { state.smb.recycleBin = v; });
    onToggle('nas-sw-tm', (v) => { state.smb.timeMachine = v; });
    onToggle('nas-sw-smbdirect', (v) => { state.smb.smbDirect = v; }, true);
    onToggle('nas-sw-audit', (v) => {
      state.smb.audit = v;
      // A share saved before the audit existed arrives with an empty group
      // list; switching the toggle on hands it the same starting point a new
      // share gets instead of an audit that audits nothing.
      if (v && !state.smb.auditGroups.length) {
        state.smb.auditGroups = ['writes'];
        state.smb.auditFailure = true;
      }
    }, true);
    for (const box of win.querySelectorAll('[data-audit-group]')) {
      box.addEventListener('change', (e) => {
        const id = box.dataset.auditGroup;
        state.smb.auditGroups = e.detail.checked
          ? [...new Set([...state.smb.auditGroups, id])]
          : state.smb.auditGroups.filter((g) => g !== id);
        draw();
      });
    }
    for (const box of win.querySelectorAll('[data-audit-result]')) {
      box.addEventListener('change', (e) => {
        if (box.dataset.auditResult === 'success') state.smb.auditSuccess = e.detail.checked;
        else state.smb.auditFailure = e.detail.checked;
        draw();
      });
    }
    for (const sel of win.querySelectorAll('[data-grant-mode]')) {
      const user = sel.dataset.grantMode;
      const grant = state.smb.users.find((u) => u.user === user);
      sel.setOptions([{ value: 'rw', label: T('shares.mode_rw') }, { value: 'ro', label: T('shares.mode_ro') }], grant?.mode || 'rw');
      sel.addEventListener('change', (e) => { if (grant) grant.mode = e.detail.value === 'ro' ? 'ro' : 'rw'; });
    }
    for (const b of win.querySelectorAll('[data-grant-remove]')) {
      b.addEventListener('click', () => { state.smb.users = state.smb.users.filter((u) => u.user !== b.dataset.grantRemove); draw(); });
    }
    const pick = win.querySelector('#nas-sw-grant-pick');
    if (pick) {
      const granted = new Set(state.smb.users.map((u) => u.user));
      const free = state.users.filter((u) => !granted.has(u.name));
      if (!free.some((u) => u.name === state.grantPick)) state.grantPick = free[0]?.name || '';
      pick.setOptions(free.map((u) => ({ value: u.name, label: u.description ? `${u.name} — ${u.description}` : u.name })), state.grantPick);
      pick.addEventListener('change', (e) => { state.grantPick = e.detail.value; });
      win.querySelector('[data-act="grant-add"]')?.addEventListener('click', () => {
        if (!state.grantPick) return;
        state.smb.users.push({ user: state.grantPick, mode: 'rw' });
        draw();
      });
    }
    win.querySelector('[data-act="manage-users"]')?.addEventListener('click', () => openShareUsersDialog(screen, {
      users: state.users,
      onChange: (list) => {
        state.users = list || state.users;
        const names = new Set(state.users.map((u) => u.name));
        state.smb.users = state.smb.users.filter((u) => names.has(u.user));
        if (win.isConnected && state.step === 1) draw();
      },
    }));

    // NFS access
    const nets = win.querySelector('#nas-sw-networks');
    if (nets) {
      const onNets = () => {
        state.networksText = nets.value;
        state.nfs.networks = parseNetworks(nets.value);
        const chips = win.querySelector('#nas-sw-network-chips');
        if (chips) chips.innerHTML = state.nfs.networks.map((n) => `<tf-chip size="sm" status="neutral" label="${escapeAttr(n)}"></tf-chip>`).join('');
        syncNext();
      };
      nets.addEventListener('input', onNets);
      nets.addEventListener('change', onNets);
    }
    onToggle('nas-sw-ro', (v) => { state.nfs.readOnly = v; });
    onToggle('nas-sw-squash', (v) => { state.nfs.rootSquash = v; });
    onToggle('nas-sw-async', (v) => { state.nfs.asyncWrites = v; }, true);
    onToggle('nas-sw-rdma', (v) => { state.nfs.rdma = v; }, true);
    onToggle('nas-sw-nfs-audit', (v) => { state.nfs.audit = v; }, true);

    // Fleet
    onToggle('nas-sw-fleet', (v) => { state.fleetMount = v; }, true);
    onToggle('nas-sw-enabled', (v) => { state.enabled = v; });

    win.querySelector('[data-wizard-cancel]')?.addEventListener('click', () => win.close());
    win.querySelector('[data-wizard-back]')?.addEventListener('click', () => { if (state.step > (editing ? 1 : 0) && !state.busy) { state.step--; draw(); } });
    win.querySelector('[data-wizard-next]')?.addEventListener('click', next);
  };

  const next = async () => {
    if (!canProceed()) return;
    if (state.step < 2) { state.step++; draw(); return; }
    await run();
  };

  const payload = () => ({
    ...(editing ? { shareId: share.shareId } : { name: state.name, protocol: state.protocol, sourcePath: state.sourcePath }),
    smb: state.protocol === 'smb' ? { guests: state.smb.guests, previousVersions: state.smb.previousVersions, recycleBin: state.smb.recycleBin, timeMachine: state.smb.timeMachine, smbDirect: state.smb.smbDirect, audit: state.smb.audit, auditGroups: state.smb.auditGroups, auditSuccess: state.smb.auditSuccess, auditFailure: state.smb.auditFailure, users: state.smb.users.map((u) => ({ user: u.user, mode: u.mode })) } : null,
    nfs: state.protocol === 'nfs' ? { networks: state.nfs.networks, readOnly: state.nfs.readOnly, rootSquash: state.nfs.rootSquash, asyncWrites: state.nfs.asyncWrites, rdma: state.nfs.rdma, audit: state.nfs.audit } : null,
    fleetMount: state.fleetMount,
    enabled: state.enabled,
  });

  const run = async () => {
    state.busy = true;
    draw();
    const kind = editing ? 'tentaNasShareUpdateRequest' : 'tentaNasShareCreateRequest';
    const title = editing ? T('wizard_share.sudo_title_edit', { name: state.name }) : T('wizard_share.sudo_title', { name: state.name });
    let res;
    try {
      res = await screen.withSudo((sudoPassword) => screen.nas(kind, { ...payload(), sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), title);
    } catch (e) {
      toast(errMessage(e), 'error');
      res = null;
    }
    state.busy = false;
    if (!res || !res.job) { if (win.isConnected) draw(); return; }
    toast(T('jobs.started', { kind: jobKindLabel(res.job.kind) }), 'success');
    win.close();
    screen.openJobLog(res.job.jobId, onDone);
  };

  win.addEventListener('close-request', () => {
    if (screen.openWindow === win) screen.openWindow = null;
  });
  draw();
  document.body.appendChild(win);
  return win;
}

// ---------------------------------------------------------------------------
// Directory browser
// ---------------------------------------------------------------------------

// Browses only what the node exposes (pool mountpoints and below). An empty
// path lists the roots; the breadcrumb walks back up.
export function openShareBrowseDialog(screen, { path = '', onPick }) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('wizard_share.browse_title'));
  win.setAttribute('icon', 'folder');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '640');
  win.setAttribute('min-width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div id="nas-sb-crumbs"></div>
      <tf-table id="nas-sb-table" empty-message="${escapeAttr(T('wizard_share.browse_empty'))}">
        <tf-column key="name" label="${escapeAttr(T('wizard_share.browse_col_name'))}" renderer="html" fill></tf-column>
        <tf-column key="shared" label="${escapeAttr(T('wizard_share.browse_col_shared'))}" renderer="html" nowrap></tf-column>
      </tf-table>
      <div class="num-err" id="nas-sb-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="confirm" disabled>${escapeHtml(T('wizard_share.browse_pick'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const state = { path: '', entries: [], current: null };
  const table = win.querySelector('#nas-sb-table');
  const confirm = win.querySelector('[data-action="confirm"]');

  const go = async (p) => {
    const err = win.querySelector('#nas-sb-error');
    err.hidden = true;
    try {
      const r = await screen.nas('tentaNasShareBrowseRequest', { path: p });
      state.path = r.path || '';
      state.entries = r.entries || [];
    } catch (e) {
      err.textContent = errMessage(e);
      err.hidden = false;
      return;
    }
    if (!win.isConnected) return;
    paint();
  };

  const paint = () => {
    const parts = state.path.split('/').filter(Boolean);
    const crumbEl = win.querySelector('#nas-sb-crumbs');
    crumbEl.innerHTML = pathCrumbsHtml(T('wizard_share.browse_root'), state.path);
    wirePathCrumbs(crumbEl, state.path, go);
    table.rows = state.entries.map((e) => ({
      _entry: e,
      name: `<div class="tf-table__cell-row">${sprite('folder')}<span class="tf-table__cell-title tf-table__cell--mono">${escapeHtml(e.name)}</span>${e.dataset ? `<tf-chip size="sm" status="info" label="${escapeAttr(e.dataset)}"></tf-chip>` : ''}</div>`,
      shared: (e.sharedAs || []).map((s) => `<tf-chip size="sm" status="accent" label="${escapeAttr(s)}"></tf-chip>`).join('') || '',
    }));
    // "Pick" returns the directory being listed; the roots list has no
    // directory of its own. The dataset mark is only known from the parent
    // listing, which `go` keeps in `state.current` when a row was clicked.
    if (!state.current || state.current.path !== state.path) {
      state.current = state.path ? { name: parts[parts.length - 1] || '', path: state.path, dataset: null } : null;
    }
    if (state.path) confirm.removeAttribute('disabled'); else confirm.setAttribute('disabled', '');
    confirm.textContent = state.path ? T('wizard_share.browse_pick_path', { path: state.path }) : T('wizard_share.browse_pick');
  };

  table.addEventListener('row-click', (e) => { state.current = e.detail.row._entry; go(e.detail.row._entry.path); });
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (!state.path || !state.current) return;
    const picked = { name: state.current.name, path: state.current.path, dataset: state.current.dataset || null };
    win.close(true);
    onPick(picked);
  });
  go(path);
  return win;
}
