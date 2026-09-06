// ===== File: modules/tentanas/target-wizard.js — the "Nowy target" wizard (n14): 1/3 type, 2/3 source and network (volume + portal interface + authentication), 3/3 summary with the security checklist =====
//
// Same window, header, progress rail and footer as the addon install wizard,
// the way share-wizard.js does it. Three steps, exactly as n14 fixes them.
//
// The iSCSI initiator allowlist is NOT here — it is a filter on a TPG and it
// belongs to the target detail. The NVMe-oF host-NQN allowlist IS here, and
// for a reason that is the opposite of a preference: nvmet keeps the
// DH-HMAC-CHAP key on the HOST object of that allowlist, so on this protocol
// the list is where the credential lives. It is offered whatever the
// authentication method is, because a target with no key still filters by NQN
// — and because hiding it while still sending it is how "no authentication"
// came to mean a subsystem that kept demanding one.
//
// The security model of §5.5 is what shapes this file:
//   (a) the portal is bound to a CHOSEN interface; 0.0.0.0 exists but needs a
//       checkbox, so it can never be the default;
//   (b) a dedicated storage interface/VLAN is recommended in the hint;
//   (c) a target with no authentication on the interface carrying the default
//       route gets a loud warning naming that interface;
//   (d) the last step says out loud that a block export is a raw disk.
// And the one-line warning of n14b says the thing everything else follows
// from: an IQN/NQN is client-declared, so only CHAP authenticates.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, ADMIN_TIMEOUT_MS, fmtBytes, errMessage, jobKindLabel } from '/js/modules/tentanas/format.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-choice-card.js';
import '/js/components/tf-checkbox.js';

// A target name becomes the tail of the IQN/NQN and a configfs directory
// component, so it is the lowercase subset both specifications allow.
const NAME_RE = /^[a-z][a-z0-9.-]{0,63}$/;
export const targetNameValid = (name) => NAME_RE.test(name);

// The naming authority of the WWNs this node generates, for the step-3 preview
// of a target that does not exist yet (an existing one carries its own `wwn`).
// It MUST equal `WWN_AUTHORITY` in tentanas/targets.rs — the server is what
// actually names the object, so a divergence here would make step 3 show an
// IQN the node never creates. A test in that module reads this file and pins
// the two together, because a constant duplicated across two languages with
// nothing checking it is a divergence waiting to happen.
export const WWN_AUTHORITY = '2026-09.local.tentaflow';

/** "1T", "500G", "2048" (bytes) → bytes. 0 when it is not a size at all. */
export function parseSize(text) {
  const m = /^\s*(\d+(?:[.,]\d+)?)\s*([kmgtpKMGTP]?)i?[bB]?\s*$/.exec(String(text || ''));
  if (!m) return 0;
  const unit = { '': 1, k: 1024, m: 1024 ** 2, g: 1024 ** 3, t: 1024 ** 4, p: 1024 ** 5 };
  return Math.round(parseFloat(m[1].replace(',', '.')) * unit[m[2].toLowerCase()]);
}

/** The methods each protocol offers, in the order the segmented control shows. */
export const AUTH_METHODS = {
  iscsi: ['chap', 'mutual-chap', 'none'],
  nvmet: ['dhchap', 'dhchap-bidi', 'none'],
};

const AUTH_LABEL_KEY = {
  chap: 'wizard_target.auth_chap',
  'mutual-chap': 'wizard_target.auth_mutual_chap',
  dhchap: 'wizard_target.auth_dhchap',
  'dhchap-bidi': 'wizard_target.auth_dhchap_bidi',
  none: 'wizard_target.auth_none',
};

/** The transports a protocol can offer on THIS node, with the probe's reason. */
export function transportOptions(protocol, caps) {
  if (protocol === 'nvmet') {
    return [
      { value: 'tcp', label: T('wizard_target.transport_tcp'), ok: true },
      { value: 'rdma', label: T('wizard_target.transport_rdma'), ok: Boolean(caps?.nvmeRdma) },
      { value: 'tcp+rdma', label: T('wizard_target.transport_tcp_rdma'), ok: Boolean(caps?.nvmeRdma) },
    ];
  }
  // iSER is a FLAG on the iSCSI portal, not a second portal: the login is TCP
  // either way and the initiator asks to switch afterwards (§5.5a).
  return [
    { value: 'tcp', label: T('wizard_target.transport_tcp'), ok: true },
    { value: 'iser', label: T('wizard_target.transport_iser'), ok: Boolean(caps?.iser) },
  ];
}

export const transportsOf = (choice) => (choice === 'tcp+rdma' ? ['tcp', 'rdma'] : [choice]);

/**
 * THE allowlist parser: one entry per line (or comma, or semicolon), trimmed,
 * blanks and duplicates dropped. Both surfaces that edit an allowlist use this
 * one — the wizard and the target detail — because they had two rules and the
 * same paste behaved differently in each.
 *
 * It does NOT change the case, and that is deliberate. An NQN is matched by
 * the kernel with `strcmp`: `nqn.…:ESX01` and `nqn.…:esx01` are two different
 * hosts. Lower-casing here silently turned the admin's paste into a string
 * that is not the client's NQN, so the client was refused at login with
 * nothing anywhere saying why. A silent fallback that costs access is worse
 * than the error it replaced — `invalidHostNqns` names it in the form instead,
 * before the sudo prompt.
 */
export const parseHostNqns = (text) => [...new Set(
  String(text || '').split(/[\n,;]+/).map((x) => x.trim()).filter(Boolean),
)];

/**
 * The entries of the allowlist the node's catalog would REFUSE, so the wizard
 * refuses them first.
 *
 * The same rule as `block::validate_nqn` / `validate_target_name`: 1..=223
 * characters of `[a-z0-9:.-]`, starting with `nqn.`, no leading dot and no
 * `..`. Mirrored rather than fetched because the check has to run on every
 * keystroke; when the two ever disagree the node is the authority and the save
 * still fails there — but after the sudo prompt, which is the whole reason
 * this exists.
 *
 * A capital IS refused here, and is meant to be: the node's alphabet is
 * lower-case only, and the alternative — quietly rewriting it — hands the
 * kernel a different host than the one the admin pasted.
 */
export function invalidHostNqns(text) {
  return parseHostNqns(text).filter(
    (n) => n.length > 223 || !/^nqn\.[a-z0-9:.-]*$/.test(n) || n.includes('..'),
  );
}

/**
 * THE addresses of an interface, in the browser — the same pair of rules the
 * node uses (`targets::bindable_addresses` / `primary_address`): every address
 * of that interface a portal can bind, and the first of them.
 *
 * They exist as one place for the reason the node's do. "The address of
 * storage0" used to be computed in three places with two different rules, and
 * an interface carrying two addresses could pass the node's drift check on one
 * of them while the summary showed the other — so the wizard promised a portal
 * the apply would not create. The LIST is what tells an alias (the saved
 * address is still one of the interface's) from a drift (it is not), which is
 * the difference between "leave this portal alone" and "this is the re-pick
 * the alert asked for".
 */
/** What "every interface" is on the wire — LIO's `np/0.0.0.0:3260`. */
export const ALL_INTERFACES_ADDRESS = '0.0.0.0';

export function bindableAddresses(caps, name) {
  return (caps?.interfaces || []).filter((i) => i.name === name && i.supported).map((i) => i.address);
}

export function primaryAddress(caps, name) {
  return bindableAddresses(caps, name)[0] || '';
}

/**
 * The transport a NEW target starts on.
 *
 * §5.5a: "RDMA domyślne, gdy sonda je wykryła". So RDMA is the starting choice
 * when the node's probe found it AND the interface the portal will bind has an
 * RDMA device — offering it on an interface without one would be a default
 * that cannot work. An edit never comes through here: it keeps what was saved.
 */
export function defaultTransport(protocol, caps, interfaceName) {
  const iface = (caps?.interfaces || []).find((i) => i.name === interfaceName);
  if (!iface || !iface.rdma) return 'tcp';
  if (protocol === 'nvmet') return caps.nvmeRdma ? 'tcp+rdma' : 'tcp';
  return caps.iser ? 'iser' : 'tcp';
}

/**
 * The authentication method a new target of this protocol starts on: the
 * MUTUAL one.
 *
 * n14 shows "CHAP mutual" as the active segment (and its step-3 summary says
 * "Mutual CHAP — obustronne uwierzytelnienie"), and the mockup is right about
 * it for a reason that outlives the mockup: one-way CHAP proves the initiator
 * to the target and nothing the other way, so an initiator cannot tell this
 * target from anything else that answered on that address. Starting on the
 * weaker of the two and letting the admin notice is the wrong default for a
 * control that hands out a raw disk.
 *
 * The list order stays as n14 draws the segmented control (`chap`,
 * `mutual-chap`, `none`); only the starting VALUE is the second one. Both
 * protocols follow the same rule — `dhchap-bidi` is NVMe-oF's mutual variant.
 */
export const defaultMethod = (protocol) => {
  const methods = AUTH_METHODS[protocol] || AUTH_METHODS.iscsi;
  return methods[1] || methods[0];
};

/**
 * Whether the wizard must show the §5.5(c) warning: no authentication on an
 * interface that carries the node's default route, i.e. the LAN. The node
 * decides which interface that is (from its routing table) — the browser only
 * repeats it.
 */
export function sharedWithoutAuth(caps, interfaceName, method) {
  if (method !== 'none') return null;
  if (!interfaceName) return { name: T('targets.all_interfaces') };
  const iface = (caps?.interfaces || []).find((i) => i.name === interfaceName);
  return iface && iface.shared ? iface : null;
}

/**
 * The other targets of this node that already allow one of these host NQNs.
 *
 * nvmet keeps the DH-HMAC-CHAP key on the HOST object, which is node-wide: a
 * subsystem only links to it. So two targets naming the same host share one
 * key, whether anybody meant them to or not — and the UI exports one LUN per
 * target (§6.1), which makes "two zvols to one VMware host" the ordinary way
 * to end up here rather than a corner case.
 *
 * The node will not silently pick a winner: a save that would put DIFFERENT
 * settings on a host another target already uses is REFUSED, with the host
 * named. What the node cannot do is say so before the admin has typed — so
 * this does, in the same place and for the same reason the volume picker says
 * "already exported by".
 *
 * Comparison is case-insensitive on both sides, so the warning names the real
 * problem rather than the alphabet.
 */
export function sharedHostTargets(targets, protocol, nqns, ownId) {
  const split = sharedHostNeighbours(targets, protocol, nqns, ownId);
  return [...split.authenticated, ...split.open];
}

/**
 * The same neighbours, SPLIT BY WHETHER THEY AUTHENTICATE — because the four
 * combinations are four different true sentences, and the UI used to tell one
 * of them in all four situations.
 *
 * The lie this closes: with two UNAUTHENTICATED targets sharing a host — the
 * ordinary §6.1 topology, one LUN per target, two zvols to one VMware host —
 * the wizard claimed in five languages that the neighbour held a
 * DH-HMAC-CHAP key on that object and that the kernel would keep demanding it.
 * There is no key. The node applies both without a murmur. And the advice that
 * followed ("turn authentication off on those targets as well") named an
 * action with nothing to act on.
 *
 * `auth.method` was already on every row (`to_protocol` sends it, and
 * `authChipHtml` renders it two lines away); this function was simply not
 * looking at it. A row with no `auth` at all counts as unauthenticated, which
 * is what the server means by an absent method.
 */
export function sharedHostNeighbours(targets, protocol, nqns, ownId) {
  if (protocol !== 'nvmet' || !nqns.length) return { authenticated: [], open: [] };
  const wanted = nqns.map((n) => String(n).trim().toLowerCase());
  const sharing = (targets || [])
    .filter((t) => t.protocol === 'nvmet' && t.targetId !== ownId)
    .filter((t) => (t.initiators || []).some((n) => wanted.includes(String(n).trim().toLowerCase())));
  return {
    authenticated: sharing.filter((t) => authenticates(t.auth)).map((t) => t.name),
    open: sharing.filter((t) => !authenticates(t.auth)).map((t) => t.name),
  };
}

/**
 * Whether a target actually puts key material on the node-wide host object.
 *
 * The METHOD is not enough, and the server says so itself: a row imported from
 * another node arrives `dhchap` with **no stored secret** (§5.8 cannot carry
 * one), the catalog refuses to render it, and `host_allowlist_conflict` skips
 * it for exactly that reason. Classifying such a neighbour as "holds a key"
 * made five locales assert a key that the server's own exemption says is not
 * there — and told the admin to match it.
 *
 * `secretSet` is on the wire on every target row (`AuthView::secret_set`), so
 * this is reading a fact the browser already has, not guessing from a method.
 * A row that omits it is treated as having one, because that is what every
 * pre-existing authenticated target looks like.
 */
export const authenticates = (auth) => {
  const method = typeof auth === 'string' ? auth : auth?.method;
  if (method !== 'dhchap' && method !== 'dhchap-bidi') return false;
  return typeof auth === 'string' ? true : auth?.secretSet !== false;
};

/**
 * The one place that turns "what do we want" + "what do the neighbours have"
 * into the sentence to show. Four combinations, four different truths:
 *
 *   * both authenticate — one object, one key: the keys must match or the node
 *     refuses THIS target at apply;
 *   * we do not, they do — their key stays on the shared object and the kernel
 *     keeps demanding it from this host, on this target too;
 *   * we do, they do not — the object carries no key today; the two rows
 *     disagree, and this one is refused at SAVE, not at apply;
 *   * neither — nothing collides. The object is shared and empty, both targets
 *     use the allowlist as a filter, and the node applies both. Informational.
 *
 * Returns `null` when there is nothing to say.
 */
export function sharedHostWarning(targets, protocol, nqns, ownId, ownAuth) {
  const { authenticated, open } = sharedHostNeighbours(targets, protocol, nqns, ownId);
  const names = [...authenticated, ...open];
  if (!names.length) return null;
  // `ownAuth` is a METHOD STRING from the wizard and the whole `auth` object
  // from the detail window, and the difference is deliberate: in the wizard
  // the admin is choosing right now and a key is typed before the save can go
  // through (`secretOk`), so the method is the truth. On a saved row the truth
  // includes whether a secret was ever stored — an imported `dhchap` row with
  // none holds nothing on the object.
  const weAuth = authenticates(ownAuth);
  let key;
  if (authenticated.length && weAuth) key = 'wizard_target.dhchap_hosts_shared';
  else if (authenticated.length) key = 'wizard_target.dhchap_hosts_shared_none';
  else if (weAuth) key = 'wizard_target.dhchap_hosts_shared_open';
  else key = 'wizard_target.dhchap_hosts_shared_plain';
  return {
    key,
    // The names in the sentence are the ones the sentence is ABOUT: a mixed
    // set says the authenticated half, because that is the half that holds
    // something.
    targets: (authenticated.length ? authenticated : open).join(', '),
    nqns: sharedHostNqns(targets, protocol, nqns, ownId).join(', '),
  };
}

/**
 * WHICH of these host NQNs the collision is about.
 *
 * The warning used to name the other targets and never the NQN — on an
 * allowlist with four entries that is the one thing the admin needs and the
 * one thing it did not say.
 */
export function sharedHostNqns(targets, protocol, nqns, ownId) {
  if (protocol !== 'nvmet' || !nqns.length) return [];
  const others = (targets || []).filter((t) => t.protocol === 'nvmet' && t.targetId !== ownId);
  // Compared lower-case, RETURNED as typed. The warning names the line the
  // admin has to go and change, so showing them a string they did not write —
  // which is what returning the folded form did — sends them looking for it.
  return nqns
    .map((n) => String(n).trim())
    .filter((n) => others.some((t) => (t.initiators || []).some((h) => String(h).trim().toLowerCase() === n.toLowerCase())));
}

export function openTargetWizard(screen, { target = null, capabilities = null, targets = [], onDone = null } = {}) {
  if (screen.openWindow) { screen.openWindow.remove(); screen.openWindow = null; }
  const node = screen.currentNode();
  const caps = capabilities || {};
  const editing = Boolean(target);
  const editPortal = (target?.portals || [])[0] || {};
  const editTransports = [...new Set((target?.portals || []).map((p) => p.transport))];
  const startProtocol = target?.protocol || (caps.iscsi === false && caps.nvmet ? 'nvmet' : 'iscsi');
  const startInterface = editing ? (editPortal.interface || '') : firstInterface(caps);
  const state = {
    step: editing ? 1 : 0,
    protocol: startProtocol,
    name: target?.name || '',
    // '' = create a new zvol. It is n14's first AND selected option: a target
    // usually means a disk that does not exist yet, and picking an existing
    // zvol is the deliberate case.
    source: target ? (target.luns || [])[0]?.source || '' : '',
    newSourceName: '',
    newSizeText: '1T',
    thin: true,
    portalInterface: startInterface,
    transport: editing
      ? (editTransports.length > 1 ? 'tcp+rdma' : editTransports[0] || 'tcp')
      : defaultTransport(startProtocol, caps, startInterface),
    // The method must belong to the protocol the wizard actually starts on: a
    // node without iSCSI starts on NVMe-oF, where 'chap' does not exist and
    // would leave the segmented control with nothing selected.
    method: target?.auth?.method || defaultMethod(startProtocol),
    username: target?.auth?.username || '',
    secret: '',
    mutualUsername: target?.auth?.mutualUsername || '',
    mutualSecret: '',
    secretSet: Boolean(target?.auth?.secretSet),
    mutualSecretSet: Boolean(target?.auth?.mutualSecretSet),
    hostNqnText: (target?.initiators || []).join('\n'),
    confirmAll: editing ? !editPortal.interface : false,
    // Opt-IN, always. The portal of an existing target moves only because
    // somebody asked, and a drifted portal is the one case where the wizard
    // has to ask rather than assume (owner decision 2026-09-04).
    movePortal: false,
    enabled: target ? Boolean(target.enabled) : true,
    busy: false,
  };
  const steps = [T('wizard_target.step_type'), T('wizard_target.step_source'), T('wizard_target.step_summary')];

  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', editing ? T('wizard_target.title_edit', { name: target.name }) : T('wizard_target.title'));
  win.setAttribute('icon', 'target');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '820');
  win.setAttribute('min-width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  screen.openWindow = win;

  const header = () => `
    <div class="install-header">
      <div class="big-ico">${sprite('target')}</div>
      <div class="install-header-meta">
        <h1>${escapeHtml(editing ? T('wizard_target.heading_edit', { name: target.name }) : T('wizard_target.heading'))} <span class="version">${escapeHtml(T('wizard.node_tag', { node: node.nodeName }))}</span></h1>
        <div class="sub">${escapeHtml(T('wizard_target.sub'))}</div>
      </div>
    </div>
    <div class="install-progress">${steps.map((s, i) => `<div class="install-step ${i === state.step ? 'active' : i < state.step ? 'done' : ''}"><span class="num">${i < state.step ? sprite('check') : i + 1}</span><span class="label">${escapeHtml(s)}</span></div>`).join('')}</div>`;

  // ----- step 1/3: the protocol (n14a) -----
  const stepType = () => {
    const unavailable = (ok, detail) => (ok ? '' : `<div class="muted">${escapeHtml(T('wizard_target.unavailable', { detail: detail || '' }))}</div>`);
    return `
      <h2 class="wizard-section-title">${escapeHtml(T('wizard_target.type_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('wizard_target.type_sub'))}</p>
      <tf-choice-group id="nas-tw-protocol" value="${escapeAttr(state.protocol)}" columns="2">
        <tf-choice-card value="iscsi" icon="target" heading="iSCSI" description="${escapeAttr(T('wizard_target.iscsi_desc'))}" ${editing || caps.iscsi === false ? 'disabled' : ''}></tf-choice-card>
        <tf-choice-card value="nvmet" icon="zap" heading="NVMe-oF" description="${escapeAttr(T('wizard_target.nvmet_desc'))}" ${editing || caps.nvmet === false ? 'disabled' : ''}></tf-choice-card>
      </tf-choice-group>
      ${unavailable(caps.iscsi !== false, caps.iscsiDetail)}
      ${unavailable(caps.nvmet !== false, caps.nvmetDetail)}
      <div class="form-grid-2 mt-md">
        <tf-input id="nas-tw-name" label="${escapeAttr(T('wizard_target.name_label'))}" placeholder="vm-store" autocomplete="off" spellcheck="false" value="${escapeAttr(state.name)}" hint="${escapeAttr(T('wizard_target.name_hint'))}" ${editing ? 'readonly' : ''}></tf-input>
      </div>`;
  };

  // ----- step 2/3: volume, portal interface, authentication (n14b) -----
  const volumeOptions = () => {
    const rows = (caps.volumes || []).map((v) => ({
      value: v.name,
      label: v.exportedBy
        ? T('wizard_target.volume_taken', { name: v.name, target: v.exportedBy })
        : T('wizard_target.volume_free', { name: v.name, size: fmtBytes(v.sizeBytes) }),
      disabled: Boolean(v.exportedBy),
    }));
    if (!rows.some((r) => !r.disabled)) {
      rows.push({ value: '__none', label: T('wizard_target.volume_none'), disabled: true });
    }
    const newName = state.newSourceName || suggestedVolume();
    rows.unshift({
      value: '',
      // The size the way every other size in this app is written, not the raw
      // characters the admin typed: n14c's contract is "1 TiB", and echoing
      // the field gave "1T". `parseSize` is the same parser the request uses,
      // so the label cannot promise a size the save would not send.
      label: T('wizard_target.volume_new', {
        name: newName,
        size: fmtBytes(parseSize(state.newSizeText)),
      }),
      disabled: false,
    });
    return rows;
  };

  // A new zvol lands in the first pool the node has, named after the target —
  // which is what makes the "+ Nowy zvol" option of n14 a single click.
  const suggestedVolume = () => {
    const pool = (caps.volumes || [])[0]?.pool || 'tank';
    return `${pool}/${state.name || 'target'}`;
  };

  const interfaceOptions = () => {
    // ONE option per interface NAME. `caps.interfaces` legitimately carries
    // the same name several times — a storage VLAN on a secondary address is
    // exactly that — and the option's value IS the name, so an alias produced
    // two `<option>`s with the same value: picking the second silently picked
    // the first, and "the .9 alias" could not be expressed at all. The portal
    // binds the interface and the node's own `primary_address` picks the
    // address, so the first entry is the one this control means; the rest of
    // the addresses belong in the label, not in a second row.
    const seen = new Set();
    const rows = (caps.interfaces || []).filter((i) => {
      if (seen.has(i.name)) return false;
      seen.add(i.name);
      return true;
    }).map((i) => ({
      value: i.name,
      label: !i.supported
        ? T('wizard_target.portal_ipv6', { name: i.name, address: i.address })
        : i.shared
          // n14 annotates the storage interface too ("VLAN storage"). The
          // dedicated one is the interface that does NOT carry the node's
          // default route — that is exactly what §5.5(b) recommends and the
          // one part of it the node can actually know, from its own routing
          // table. Gating this on an RDMA device (as it was) hid the
          // annotation from every node whose storage VLAN is ordinary
          // Ethernet, which is most of them.
          ? T('wizard_target.portal_option_shared', { name: i.name, address: bindableAddresses(caps, i.name).join(', ') })
          : T('wizard_target.portal_option_storage', { name: i.name, address: bindableAddresses(caps, i.name).join(', ') }),
      // Listed so an IPv6-only node sees WHY it has nothing to pick, rather
      // than an empty control.
      disabled: !i.supported,
    }));
    // Last, never first: 0.0.0.0 is a decision, not a default (§5.5a).
    rows.push({ value: '', label: T('wizard_target.portal_all') });
    return rows;
  };

  // Everything that depends on WHAT IS TYPED in the allowlist, and nothing
  // else. Rendered into its own container so a keystroke repaints this and not
  // the whole step: `draw()` replaces the wizard's `innerHTML`, which took the
  // caret out of the field on every character typed.
  const hostAllowlistWarnings = () => {
    // ONE place picks the sentence — `sharedHostWarning` — because picking it
    // from `state.method` alone told two of the four combinations wrong.
    const shared = sharedHostWarning(targets, state.protocol, parseHostNqns(state.hostNqnText), target?.targetId, state.method);
    const invalid = invalidHostNqns(state.hostNqnText);
    return `
      ${shared ? `<div class="wizard-warning">${sprite('alert')}<div>${escapeHtml(T(shared.key, { nqns: shared.nqns, targets: shared.targets }))}</div></div>` : ''}
      ${invalid.length ? `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_target.host_nqn_invalid', { nqns: invalid.join(', ') }))}</div></div>` : ''}
      ${state.method === 'none' || parseHostNqns(state.hostNqnText).length ? '' : `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_target.dhchap_hosts_required'))}</div></div>`}`;
  };

  // The NQN allowlist is NOT an auth field, and used to be rendered as one.
  // Switching an nvmet target to "no authentication" hid this control, but the
  // save still sent `state.hostNqnText` — so the node kept an allowlist the
  // admin could no longer see, and, when a host on it is shared with an
  // authenticated target, kept demanding a key on a target the UI called
  // unauthenticated. §5.5 calls the allowlist "a filter, not a login": it is
  // legal without a key, so it is shown without one.
  const hostAllowlistFields = () => {
    if (state.protocol !== 'nvmet') return '';
    return `
      <tf-input id="nas-tw-hosts" multiline rows="2" label="${escapeAttr(T('wizard_target.dhchap_hosts_label'))}" spellcheck="false" placeholder="nqn.2014-08.org.nvmexpress:uuid:…" value="${escapeAttr(state.hostNqnText)}" hint="${escapeAttr(T('wizard_target.dhchap_hosts_hint'))}"></tf-input>
      <div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(T(state.method === 'none' ? 'wizard_target.dhchap_hosts_filter_note' : 'wizard_target.dhchap_hosts_note'))}</div></div>
      <div id="nas-tw-hosts-warn">${hostAllowlistWarnings()}</div>`;
  };

  const authFields = () => {
    if (state.method === 'none') return '';
    if (state.protocol === 'nvmet') {
      return `
        <div class="form-grid-2 mt-md">
          <tf-input id="nas-tw-secret" type="password" label="${escapeAttr(T('wizard_target.dhchap_key'))}" autocomplete="new-password" placeholder="DHHC-1:00:…" value="${escapeAttr(state.secret)}" hint="${escapeAttr(T('wizard_target.dhchap_key_hint'))}"></tf-input>
          ${state.method === 'dhchap-bidi' ? `<tf-input id="nas-tw-msecret" type="password" label="${escapeAttr(T('wizard_target.dhchap_ctrl_key'))}" autocomplete="new-password" placeholder="DHHC-1:00:…" value="${escapeAttr(state.mutualSecret)}"></tf-input>` : ''}
        </div>
        <div class="muted">${escapeHtml(T('targets.key_in_kernel'))}</div>`;
    }
    return `
      <div class="form-grid-2 mt-md">
        <tf-input id="nas-tw-user" label="${escapeAttr(T('wizard_target.auth_user'))}" autocomplete="off" spellcheck="false" value="${escapeAttr(state.username)}"></tf-input>
        <tf-input id="nas-tw-secret" type="password" label="${escapeAttr(T('wizard_target.auth_secret'))}" autocomplete="new-password" value="${escapeAttr(state.secret)}" hint="${escapeAttr(T('wizard_target.auth_secret_hint'))}"></tf-input>
      </div>
      ${state.method === 'mutual-chap' ? `
      <div class="form-grid-2">
        <tf-input id="nas-tw-muser" label="${escapeAttr(T('wizard_target.auth_mutual_user'))}" autocomplete="off" spellcheck="false" value="${escapeAttr(state.mutualUsername)}"></tf-input>
        <tf-input id="nas-tw-msecret" type="password" label="${escapeAttr(T('wizard_target.auth_mutual_secret'))}" autocomplete="new-password" value="${escapeAttr(state.mutualSecret)}" hint="${escapeAttr(T('wizard_target.auth_mutual_hint'))}"></tf-input>
      </div>` : ''}`;
  };

  const stepSource = () => {
    const methods = AUTH_METHODS[state.protocol] || AUTH_METHODS.iscsi;
    const dhchapOff = state.protocol === 'nvmet' && caps.dhchap === false;
    const shared = sharedWithoutAuth(caps, state.portalInterface, state.method);
    const transports = transportOptions(state.protocol, caps);
    const chosen = transports.find((t) => t.value === state.transport);
    return `
      <h2 class="wizard-section-title">${escapeHtml(T('wizard_target.source_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('wizard_target.source_sub'))}</p>
      <div class="form-grid-2">
        <div class="field" style="margin-bottom:0;">
          <label>${escapeHtml(T('wizard_target.volume_label'))}</label>
          <tf-select id="nas-tw-volume" ${editing ? 'disabled' : ''}></tf-select>
        </div>
        <div class="field" style="margin-bottom:0;">
          <label>${escapeHtml(T('wizard_target.portal_label'))}</label>
          <tf-select id="nas-tw-iface"></tf-select>
        </div>
      </div>
      ${!editing && state.source === '' ? `
      <div class="form-grid-2 mt-md">
        <tf-input id="nas-tw-newsize" label="${escapeAttr(T('wizard_target.new_size_label'))}" autocomplete="off" spellcheck="false" value="${escapeAttr(state.newSizeText)}" hint="${escapeAttr(T('wizard_target.new_size_hint'))}"></tf-input>
        <tf-input id="nas-tw-newname" label="${escapeAttr(T('wizard_target.volume_label'))}" autocomplete="off" spellcheck="false" value="${escapeAttr(state.newSourceName || suggestedVolume())}"></tf-input>
      </div>` : ''}
      <div class="hint mt-sm">${escapeHtml(T('wizard_target.portal_hint'))}</div>
      <div class="field mt-md" style="margin-bottom:0;">
        <label>${escapeHtml(T('wizard_target.transport_label'))}</label>
        <tf-segmented id="nas-tw-transport" value="${escapeAttr(state.transport)}" size="sm">
          ${transports.map((t) => `<option value="${escapeAttr(t.value)}" ${t.ok ? '' : 'disabled'}>${escapeHtml(t.label)}</option>`).join('')}
        </tf-segmented>
        ${chosen && !chosen.ok ? `<div class="muted">${escapeHtml(T('wizard_target.transport_unavailable', { detail: caps.rdmaDetail || '' }))}</div>` : ''}
      </div>
      <div class="field mt-md" style="margin-bottom:0;">
        <label>${escapeHtml(T('wizard_target.auth_label'))}</label>
        <tf-segmented id="nas-tw-auth" value="${escapeAttr(state.method)}" size="sm">
          ${methods.map((m) => `<option value="${escapeAttr(m)}" ${dhchapOff && m !== 'none' ? 'disabled' : ''}>${escapeHtml(T(AUTH_LABEL_KEY[m]))}</option>`).join('')}
        </tf-segmented>
        ${dhchapOff ? `<div class="muted">${escapeHtml(T('wizard_target.dhchap_unavailable', { detail: caps.dhchapDetail || '' }))}</div>` : ''}
      </div>
      ${authFields()}
      ${hostAllowlistFields()}
      ${state.protocol === 'nvmet' ? `<div class="muted mt-sm">${escapeHtml(T('wizard_target.tls_later'))}</div>` : ''}
      <div class="wizard-warning mt-md">${sprite('alert')}<div>${escapeHtml(state.protocol === 'nvmet' ? T('wizard_target.warn_nqn') : T('wizard_target.warn_iqn'))}</div></div>
      ${state.portalInterface === '' ? `
        <div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_target.warn_all_interfaces'))}</div></div>
        <tf-checkbox id="nas-tw-confirm-all" label="${escapeAttr(T('wizard_target.confirm_all_interfaces'))}" ${state.confirmAll ? 'checked' : ''}></tf-checkbox>` : ''}
      ${shared ? `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_target.warn_no_auth_shared', { iface: shared.name }))}</div></div>` : ''}
      ${interfaceGone() ? `<div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_target.portal_interface_gone', { iface: state.portalInterface }))}</div></div>` : ''}
      ${offerPortalMove() ? `
        <div class="wizard-warning danger">${sprite('alert')}<div>${escapeHtml(T('wizard_target.portal_drifted', { iface: state.portalInterface, from: savedAddress(), to: primaryAddress(caps, state.portalInterface) }))}</div></div>
        <tf-checkbox id="nas-tw-move-portal" label="${escapeAttr(T('wizard_target.portal_move_confirm', { to: primaryAddress(caps, state.portalInterface) }))}" ${state.movePortal ? 'checked' : ''}></tf-checkbox>` : ''}`;
  };

  // ----- step 3/3: the summary and the security checklist (n14c) -----
  const chosenVolume = () => {
    if (state.source) return (caps.volumes || []).find((v) => v.name === state.source) || { name: state.source, sizeBytes: 0, thin: true };
    return { name: state.newSourceName || suggestedVolume(), sizeBytes: parseSize(state.newSizeText), thin: state.thin, isNew: true };
  };
  const chosenInterface = () => (caps.interfaces || []).find((i) => i.name === state.portalInterface && i.supported);
  /** Where the portal is today, when editing — '' for a target being created. */
  const savedAddress = () => (editing ? ((target.portals || [])[0] || {}).address || '' : '');
  /**
   * Whether the admin changed the INTERFACE the portal is bound to.
   *
   * This — and only this — is what makes the save carry `repickPortal`. The
   * flag is an INTENT, not a value: it says "move the portal", so an edit that
   * never touched step 2 must not carry it. It used to be unconditional, and
   * on an interface with two addresses that meant changing a CHAP secret moved
   * a live portal onto the interface's primary address and `rmdir`-ed the old
   * one under every initiator logged in on it. The wizard warned about it in
   * red, but offered no way to say no — and a warning with no alternative is
   * not a decision.
   *
   * The TRANSPORT is deliberately not part of this. It is not an address:
   * switching TCP to iSER changes what the portal speaks, not where it
   * listens, and the node keeps the stored address for an interface it already
   * has (see `target_update`).
   */
  const interfaceChanged = () => editing && state.portalInterface !== (editPortal.interface || '');
  /**
   * The portal's address is no longer one its interface holds — the drift the
   * alert asks an admin to repair by re-picking the interface.
   *
   * This is the OTHER half of the re-pick intent, and it uses the same rule
   * the node's drift check uses (the whole LIST of the interface's addresses,
   * not just the first). Without it, opening the wizard on a drifted target
   * and pressing Save would change nothing at all, because the interface name
   * did not change — the one repair the alert names would be a no-op.
   *
   * And with the list rather than the first address, an ALIAS is not drift: a
   * target sitting on an interface's second address is healthy, is left alone,
   * and a CHAP-secret edit does not move it.
   */
  const portalDrifted = () =>
    editing
    && Boolean(editPortal.interface)
    && Boolean(savedAddress())
    && !bindableAddresses(caps, editPortal.interface).includes(savedAddress());
  /**
   * Whether this save asks the node to MOVE the portal.
   *
   * Two ways to get here, and only one of them is automatic:
   *   * the admin picked a different interface — that IS the move, there is
   *     nothing to ask;
   *   * the portal drifted and the admin ticked the box next to the warning.
   *
   * The tick-box is the whole point of the second case. A frozen target used
   * to be uneditable without moving its portal: changing its CHAP secret, or
   * (for NVMe-oF, whose host allowlist lives in this wizard) taking a host off
   * it, carried the re-pick whether the admin wanted it or not. The node has
   * always been able to take a save that leaves the portal alone —
   * `portals_for_update` with no intent keeps the address the ROW holds — it
   * was the wizard that could not send one.
   */
  const portalMoves = () => interfaceChanged() || (portalDrifted() && state.movePortal);
  /** Whether to OFFER the move: it drifted, and the admin has not re-picked. */
  const offerPortalMove = () => portalDrifted() && !interfaceChanged() && !interfaceGone();
  /**
   * The address the portal has NOW and the one it will have — as a pair, or
   * `null` when it does not move.
   *
   * Owner decision (2026-09-04): a block export never re-plumbs itself, so the
   * wizard is the only place a portal can change address — and an admin who is
   * about to move one has to be told, because every initiator logged in on the
   * old address loses its path the moment the old portal is removed.
   *
   * `0.0.0.0` is one of the two addresses here, in both directions. Narrowing
   * an every-interface target down to one interface drops `np/0.0.0.0:3260`,
   * which is EVERY logged-in initiator — the loudest version of this warning,
   * and the one it used to skip because one side of the comparison was empty.
   */
  const portalMove = () => {
    if (!editing || !portalMoves()) return null;
    const now = savedAddress() || ALL_INTERFACES_ADDRESS;
    const next = state.portalInterface
      ? primaryAddress(caps, state.portalInterface)
      : ALL_INTERFACES_ADDRESS;
    return next && now !== next ? { from: now, to: next } : null;
  };
  /**
   * The interface the target is bound to is no longer one this node has an
   * address for.
   *
   * It does NOT block the save. The node accepts an edit that leaves the
   * portal alone (`portals_for_update` keeps the row's own address when no
   * intent is sent), and blocking here made a target whose interface had
   * vanished impossible to edit AT ALL — not its secret, not its transport,
   * not its allowlist. What it does is take the move off the table, because
   * there is no address to move to, and say so.
   *
   * This can only be true of the SAVED interface: the picker offers nothing
   * else. So it never contradicts a choice the admin just made.
   */
  const interfaceGone = () =>
    Boolean(state.portalInterface) && primaryAddress(caps, state.portalInterface) === '';
  const wwnPreview = () => {
    const prefix = state.protocol === 'nvmet' ? 'nqn' : 'iqn';
    const host = (node.nodeName || '').toLowerCase().replace(/[^a-z0-9-]/g, '');
    return editing ? target.wwn : `${prefix}.${WWN_AUTHORITY}:${host}.${state.name}`;
  };

  const checklist = () => {
    const iface = chosenInterface();
    const rows = [];
    rows.push(state.portalInterface
      ? { ok: true, text: T('wizard_target.check_iface', { iface: state.portalInterface }) }
      : { ok: false, text: T('wizard_target.check_iface_all') });
    const authRow = {
      'mutual-chap': { ok: true, text: T('wizard_target.check_auth_mutual') },
      chap: { ok: true, text: T('wizard_target.check_auth_chap') },
      dhchap: { ok: true, text: T('wizard_target.check_auth_dhchap') },
      'dhchap-bidi': { ok: true, text: T('wizard_target.check_auth_dhchap_bidi') },
      none: { ok: false, text: T('wizard_target.check_auth_none') },
    }[state.method];
    rows.push(authRow);
    rows.push({ ok: true, text: T('wizard_target.check_persistence') });
    if (iface && iface.shared && state.method === 'none') {
      rows.push({ ok: false, text: T('wizard_target.warn_no_auth_shared', { iface: iface.name }) });
    }
    return `<ul class="loss-list mt-md">${rows.map((r) => `<li class="ll ${r.ok ? 'good' : 'bad'}">${sprite(r.ok ? 'check' : 'alert')}<span>${escapeHtml(r.text)}</span></li>`).join('')}</ul>`;
  };

  const stepSummary = () => {
    const volume = chosenVolume();
    const port = state.protocol === 'nvmet' ? 4420 : 3260;
    // The interface's address through the ONE definition of that phrase, so
    // the summary can never promise a portal the node would build differently.
    const address = state.portalInterface
      ? primaryAddress(caps, state.portalInterface)
      : ALL_INTERFACES_ADDRESS;
    const moved = portalMove();
    const transports = transportsOf(state.transport).map((t) => T(`wizard_target.transport_${t}`)).join(' + ');
    return `
      <h2 class="wizard-section-title">${escapeHtml(T('wizard_target.summary_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('wizard_target.summary_sub'))}</p>
      <div class="stat-rows">
        <div class="sr"><span class="k">${escapeHtml(T('wizard_target.sum_target'))}</span><span class="v mono" id="nas-tw-sum-wwn">${escapeHtml(wwnPreview())}</span></div>
        <div class="sr"><span class="k">${escapeHtml(state.protocol === 'nvmet' ? T('wizard_target.sum_namespace') : T('wizard_target.sum_lun'))}</span><span class="v"><span class="mono">zvol ${escapeHtml(volume.name)}</span> · ${escapeHtml(fmtBytes(volume.sizeBytes))}${volume.thin ? ' · thin' : ''}${volume.isNew ? ` · ${escapeHtml(T('wizard_target.sum_new_volume'))}` : ''}</span></div>
        <div class="sr"><span class="k">${escapeHtml(T('wizard_target.sum_portal'))}</span><span class="v"><span class="mono">${escapeHtml(address)}:${port}</span> · ${escapeHtml(state.portalInterface || T('targets.all_interfaces'))} · ${escapeHtml(transports)}</span></div>
        <div class="sr"><span class="k">${escapeHtml(T('wizard_target.sum_auth'))}</span><span class="v" id="nas-tw-sum-auth">${escapeHtml(T(AUTH_LABEL_KEY[state.method]))}</span></div>
      </div>
      ${checklist()}
      ${moved ? `<div class="wizard-warning danger mt-md">${sprite('alert')}<div>${escapeHtml(T('wizard_target.portal_moves', { from: moved.from, to: moved.to }))}</div></div>` : ''}
      ${(() => {
        // Repeated here, like every other warning about a CONSEQUENCE of the
        // save. This is the last screen before the node is asked to do it,
        // and this particular consequence lands on a target the admin is not
        // editing and cannot see from here.
        const shared = sharedHostWarning(targets, state.protocol, parseHostNqns(state.hostNqnText), target?.targetId, state.method);
        if (!shared) return '';
        return `<div class="wizard-warning mt-md">${sprite('alert')}<div>${escapeHtml(T(shared.key, { nqns: shared.nqns, targets: shared.targets }))}</div></div>`;
      })()}
      <div class="wizard-warning danger mt-md">${sprite('alert')}<div>${escapeHtml(T('wizard_target.warn_raw_disk'))}</div></div>`;
  };

  const secretOk = () => {
    // An edit keeps the stored secret when the field is left empty; a new
    // target has to be given one.
    if (state.method === 'none') return true;
    const primary = state.secret || (editing && state.secretSet);
    const mutualNeeded = state.method === 'mutual-chap' || state.method === 'dhchap-bidi';
    const mutual = !mutualNeeded || state.mutualSecret || (editing && state.mutualSecretSet);
    if (state.protocol === 'nvmet') {
      // nvmet reads DH-HMAC-CHAP keys off the HOST objects the allowlist is
      // made of, so an authenticated subsystem with no host NQN is refused by
      // the node — the wizard stops before the request rather than after the
      // error, and before the zvol would have been created.
      return Boolean(primary) && Boolean(mutual) && parseHostNqns(state.hostNqnText).length > 0;
    }
    return Boolean(primary) && Boolean(mutual) && Boolean(state.username);
  };

  const canProceed = () => {
    if (state.busy) return false;
    if (state.step === 0) return targetNameValid(state.name);
    if (state.step === 1) {
      if (!state.source && parseSize(state.newSizeText) <= 0) return false;
      // §5.5(a): every interface is possible and never silent.
      if (state.portalInterface === '' && !state.confirmAll) return false;
      const transports = transportOptions(state.protocol, caps);
      if (!transports.some((t) => t.value === state.transport && t.ok)) return false;
      // Shape-checked here and not only in `secretOk`, because the allowlist
      // is offered on the unauthenticated path too and the node refuses a
      // malformed NQN there just the same.
      if (state.protocol === 'nvmet' && invalidHostNqns(state.hostNqnText).length) return false;
      return secretOk();
    }
    return true;
  };

  const footer = () => {
    const last = state.step === 2;
    const first = state.step === (editing ? 1 : 0);
    const next = last
      ? `<tf-button variant="primary" icon="check" data-wizard-next ${canProceed() ? '' : 'disabled'}>${escapeHtml(editing ? T('wizard_target.save_button') : T('wizard_target.create_button'))}</tf-button>`
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
        <div class="install-step-body">${[stepType, stepSource, stepSummary][state.step]()}</div>
      </div>
      <div slot="footer">${footer()}</div>`;
    wire();
  };

  const onText = (id, apply, redraw = false) => {
    const el = win.querySelector('#' + id);
    if (!el) return;
    const handler = () => { apply(el.value); if (redraw) draw(); else syncNext(); };
    el.addEventListener('input', handler);
    el.addEventListener('change', handler);
  };

  const wire = () => {
    win.querySelector('#nas-tw-protocol')?.addEventListener('change', (e) => {
      state.protocol = e.detail.value;
      // Each protocol has its own methods and its own transports; carrying the
      // old choice over would offer CHAP on NVMe-oF.
      state.method = defaultMethod(state.protocol);
      state.transport = defaultTransport(state.protocol, caps, state.portalInterface);
      // A CHAP password is not a DH-HMAC-CHAP key and the reverse: carrying
      // the typed secret across the switch offered it to the OTHER protocol's
      // field, where the catalog refuses its shape — and, on a wizard left
      // open, kept a credential the admin had typed for something else.
      state.secret = '';
      state.mutualSecret = '';
      draw();
    });
    onText('nas-tw-name', (v) => {
      state.name = v.trim();
      const el = win.querySelector('#nas-tw-name');
      if (state.name && !targetNameValid(state.name)) el.setAttribute('error', T('wizard_target.name_invalid'));
      else el.removeAttribute('error');
    });

    const volume = win.querySelector('#nas-tw-volume');
    if (volume) {
      volume.setOptions(volumeOptions(), state.source);
      volume.addEventListener('change', (e) => { state.source = e.detail.value; draw(); });
    }
    const iface = win.querySelector('#nas-tw-iface');
    if (iface) {
      iface.setOptions(interfaceOptions(), state.portalInterface);
      iface.addEventListener('change', (e) => {
        state.portalInterface = e.detail.value;
        if (state.portalInterface) state.confirmAll = false;
        // The transport follows the interface: RDMA is only a sensible default
        // on an interface that has an RDMA device (§5.5a).
        if (!editing) state.transport = defaultTransport(state.protocol, caps, state.portalInterface);
        draw();
      });
    }
    onText('nas-tw-newsize', (v) => { state.newSizeText = v; }, false);
    onText('nas-tw-newname', (v) => { state.newSourceName = v.trim(); });
    win.querySelector('#nas-tw-transport')?.addEventListener('change', (e) => { state.transport = e.detail.value; draw(); });
    win.querySelector('#nas-tw-auth')?.addEventListener('change', (e) => { state.method = e.detail.value; draw(); });
    win.querySelector('#nas-tw-confirm-all')?.addEventListener('change', (e) => {
      state.confirmAll = Boolean(e.detail?.checked ?? e.target.checked);
      syncNext();
    });
    win.querySelector('#nas-tw-move-portal')?.addEventListener('change', (e) => {
      state.movePortal = Boolean(e.detail?.checked ?? e.target.checked);
      syncNext();
    });
    onText('nas-tw-hosts', (v) => {
      state.hostNqnText = v;
      const box = win.querySelector('#nas-tw-hosts-warn');
      if (box) box.innerHTML = hostAllowlistWarnings();
    });
    onText('nas-tw-user', (v) => { state.username = v.trim(); });
    onText('nas-tw-secret', (v) => { state.secret = v; });
    onText('nas-tw-muser', (v) => { state.mutualUsername = v.trim(); });
    onText('nas-tw-msecret', (v) => { state.mutualSecret = v; });

    win.querySelector('[data-wizard-cancel]')?.addEventListener('click', () => win.close());
    win.querySelector('[data-wizard-back]')?.addEventListener('click', () => { if (state.step > (editing ? 1 : 0) && !state.busy) { state.step--; draw(); } });
    win.querySelector('[data-wizard-next]')?.addEventListener('click', next);
  };

  const next = async () => {
    if (!canProceed()) return;
    if (state.step < 2) { state.step++; draw(); return; }
    await run();
  };

  const auth = () => {
    if (state.method === 'none') return { method: 'none' };
    return {
      method: state.method,
      username: state.protocol === 'nvmet' ? '' : state.username,
      // An empty secret on an edit means "keep the stored one"; the node knows.
      secret: state.secret || null,
      mutualUsername: state.protocol === 'nvmet' ? '' : state.mutualUsername,
      mutualSecret: state.mutualSecret || null,
    };
  };

  const payload = () => {
    const volume = chosenVolume();
    if (editing) {
      return {
        targetId: target.targetId,
        portals: transportsOf(state.transport).map((t) => ({
          interface: state.portalInterface,
          // Left empty on purpose: the NODE decides what address an interface
          // has. The browser saying so would be a portal pointed by a request.
          address: '',
          port: state.protocol === 'nvmet' ? 4420 : 3260,
          transport: t,
        })),
        // ONLY when the admin actually changed the interface in step 2, and
        // the summary above then says which address the portal moves from and
        // to. An edit that did not touch the portal does not carry the intent
        // at all — that is the whole point of the flag being an intent rather
        // than a value, and sending it unconditionally made a CHAP-secret edit
        // move a live portal on any interface with a second address.
        //
        // A transport-only change still sends its portals, without the flag:
        // the node keeps the stored address for an interface it already holds,
        // because a transport is not an address.
        ...(portalMoves() ? { repickPortal: true } : {}),
        auth: auth(),
        // The host-NQN field is rendered on an EDIT too, and `secretOk()`
        // gates the wizard on it — sending the stored list back would throw
        // away what the admin just typed, report success, and (going from
        // "None" to DH-HMAC-CHAP) make the path impossible to walk at all:
        // nvmet keeps the keys on the host objects of the allowlist, so the
        // node refuses an authenticated subsystem with no host NQN.
        initiators: state.protocol === 'nvmet' ? parseHostNqns(state.hostNqnText) : (target.initiators || []),
        portGroups: target.portGroups || [],
        confirmAllInterfaces: state.confirmAll,
        enabled: state.enabled,
      };
    }
    return {
      name: state.name,
      protocol: state.protocol,
      source: volume.name,
      createSizeBytes: volume.isNew ? volume.sizeBytes : 0,
      thin: volume.isNew ? state.thin : Boolean(volume.thin),
      portalInterface: state.portalInterface,
      transports: transportsOf(state.transport),
      auth: auth(),
      initiators: state.protocol === 'nvmet' ? parseHostNqns(state.hostNqnText) : [],
      confirmAllInterfaces: state.confirmAll,
      enabled: state.enabled,
    };
  };

  const run = async () => {
    state.busy = true;
    draw();
    const kind = editing ? 'tentaNasTargetUpdateRequest' : 'tentaNasTargetCreateRequest';
    const title = editing ? T('wizard_target.sudo_title_edit', { name: state.name || target.name }) : T('wizard_target.sudo_title', { name: state.name });
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

/** The interface the picker starts on: a dedicated (non-LAN) one when the node
 *  has one, because that is what §5.5(b) recommends; otherwise the first. */
function firstInterface(caps) {
  // Only an interface a portal can actually bind, and preferably one that is
  // not the LAN (§5.5b).
  const list = (caps.interfaces || []).filter((i) => i.supported);
  return (list.find((i) => !i.shared) || list[0] || { name: '' }).name;
}
