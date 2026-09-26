// =============================================================================
// File: modules/addons/uninstall-dialog.js
// Description: The uninstall confirmation for an addon instance (admin). Opens
//              a tf-window, fetches the side-effect-free teardown plan
//              (AddonTeardownPlanRequest) and lists what the wipe removes and
//              what it consciously keeps — as words and sizes, never as
//              paths (an instance's paths carry its id) — and the instances
//              that depend on the package. The danger button unlocks only
//              after the admin retypes the instance name; confirm sends
//              AddonUninstallRequest. Shared by the card action and the detail
//              header so both flows show the same facts.
//
//              An instance that lives on several nodes (n18a, MAJOR 22) gets a
//              row per node, by its real name: its privilege mode (A/B), what
//              the uninstall does THERE (each node's own plan, forwarded to
//              it), the configuration backup it writes, and — once the admin
//              confirmed — that node's own progress, result and error, worded.
//
//              The confirm stays locked until EVERY node with work is known
//              not to refuse (wave-9b critic, MAJOR 1): its own plan arrived
//              and blocks nothing, or — for a node the mesh cannot reach — the
//              blockers it last published are known and empty (the uninstall
//              then runs when it returns). A node still loading, or whose plan
//              is unknown, keeps it locked, with a "retry" on its row. The
//              node refuses the same cases on its own before anything
//              replicates (`teardown_preflight`).
//
//              A mode-B node (no unattended privilege channel) gets its own
//              sudo password field: the password arms that node's channel
//              (AddonTeardownArmRequest, forwarded) right before the
//              uninstall, so its teardown can export its pools cleanly.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast, formatBytes } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { setText, setAttr } from '/js/lib/dom-patch.js';
import '/js/components/tf-window.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';

const t = (key, vars) => I18n.t(`addon_uninstall.${key}`, vars);

/** How often the per-node rows ask their node where the uninstall stands. */
const STATUS_POLL_MS = 2000;
/** How long the rows are followed before they settle on what they last read. */
const STATUS_FOLLOW_MS = 10 * 60 * 1000;
/** A node that has finished, one way or the other. */
const FINAL_STATES = new Set(['done', 'failed', 'absent']);
/**
 * The teardown steps in which a node refuses because of what its plan calls
 * blocking (`NativeAppHooks::refusable_teardown`): a failure there is that
 * blocker, as the node last published it or as its plan said.
 */
const REFUSAL_PHASES = new Set(['tentanas_elastic_check']);
/** The refusals a node answers BEFORE the removal replicates. */
const NOT_STARTED = /refusal:(teardown_[a-z_]+)/;

// Localized label for a plan entry. A kind whose words interpolate counts
// (`{n}`) needs them: without counts (an older node's plan, or a count the
// node left out because it is zero) the `<kind>_plain` words are used, and
// only a kind with no words at all shows the node's English description —
// never a raw placeholder.
function entryLabel(entry) {
  const key = `addon_uninstall.entries.${entry.kind}`;
  const label = I18n.t(key, entry.countVars);
  if (label !== key && !/\{\w+(\|[^}]*)?\}/.test(label)) return label;
  const plainKey = `${key}_plain`;
  const plain = I18n.t(plainKey);
  if (plain !== plainKey) return plain;
  return label === key || /\{\w+/.test(label) ? String(entry.description || '') : label;
}

/** A stable code of the node, in the reader's language; '' when unknown. */
function worded(group, code) {
  if (!code) return '';
  const key = `addon_uninstall.${group}.${code}`;
  const words = I18n.t(key);
  return words === key ? '' : words;
}

// No path is printed (MAJOR 3): the per-instance ones carry the instance id,
// and the label already says what the entry is and where it lives.
function renderEntries(entries, removed) {
  const rows = entries.filter((e) => !e.blocks && !!e.removed === removed);
  if (rows.length === 0) return '';
  const title = removed ? t('will_remove') : t('will_keep');
  const items = rows.map((e) => `
    <li class="uninstall-entry ${removed ? 'removed' : 'kept'}">
      <div class="uninstall-entry-label">${escapeHtml(entryLabel(e))}</div>
      ${e.sizeBytes > 0 ? `<div class="uninstall-entry-path">${escapeHtml(formatBytes(e.sizeBytes))}</div>` : ''}
    </li>`).join('');
  return `
    <div class="uninstall-section">
      <div class="uninstall-section-title">${escapeHtml(title)}</div>
      <ul class="uninstall-entries">${items}</ul>
    </div>`;
}

function renderDependents(dependents) {
  if (!dependents.length) return '';
  const names = dependents.map((d) => `<b>${escapeHtml(d.displayName)}</b>${d.optional ? ` (${escapeHtml(t('dependent_optional'))})` : ''}`).join(', ');
  return `
    <div class="alert warn">
      <svg class="icon"><use href="#i-alert"/></svg>
      <div>${t('dependents', { names })}</div>
    </div>`;
}

/** A node as the screen names it: its name, or words — never its id. */
function nodeName(node) {
  const name = String(node?.name || '').trim();
  return name || t('node_unnamed');
}

// The nodes the uninstall reaches. A plan from a node of an older build has
// no `nodes`: the dialog then shows no table, exactly as before.
function fleetNodes(plan) {
  return Array.isArray(plan.nodes) && plan.nodes.length > 1 ? plan.nodes : [];
}

/** Whether a node has anything to tear down at all. */
function nodeHasWork(node) {
  // An unpaired node (no longer a trusted peer) never receives the removal:
  // nothing to do there, and it holds nothing back (MAJOR A, round 3).
  return node.status !== 'unsupported' && !node.unpaired;
}

/** What the admin retypes to proceed without a node: its name, or LOST. */
function ackWord(node) {
  const name = String(node?.name || '').trim();
  return name || 'LOST';
}

// The password field of a mode-B node (n18a). Plain markup, created once per
// row when that node's plan says it needs one.
function passwordFieldHtml(id, node) {
  return `<label class="uninstall-node-password">
      <span>${escapeHtml(t('password_prompt'))}</span>
      <input class="input" type="password" autocomplete="off" data-role="password" id="${escapeAttr(id)}" placeholder="${escapeAttr(t('password_placeholder', { node: nodeName(node) }))}">
      <span class="detail" data-role="password-error"></span>
    </label>`;
}

function renderNodeTable(nodes) {
  if (!nodes.length) return '';
  const rows = nodes.map((n) => `
    <tr data-node="${escapeAttr(n.nodeId)}">
      <td>
        <div class="uninstall-node-name">${escapeHtml(nodeName(n))}</div>
        <div class="uninstall-node-meta"><tf-chip size="sm" data-role="node-chip"></tf-chip> <tf-chip size="sm" data-role="mode-chip" hidden></tf-chip></div>
      </td>
      <td>
        <div class="uninstall-node-scope" data-role="scope"></div>
        <tf-button size="sm" variant="ghost" icon="refresh" data-act="retry" hidden>${escapeHtml(t('retry'))}</tf-button>
        <label class="uninstall-node-ack" data-role="ack" hidden>
          <span>${escapeHtml(t('ack_prompt', { word: ackWord(n) }))}</span>
          <input class="input mono" type="text" autocomplete="off" spellcheck="false" data-role="ack-input" placeholder="${escapeAttr(ackWord(n))}">
        </label>
        <div data-role="password-slot"></div>
      </td>
      <td><div class="uninstall-node-backup"><tf-chip size="sm" data-role="backup-chip" hidden></tf-chip><span class="mono detail" data-role="backup-file"></span></div></td>
      <td><div class="uninstall-node-state"><tf-chip size="sm" data-role="state-chip"></tf-chip><span class="detail" data-role="state-detail"></span></div></td>
    </tr>`).join('');
  return `
    <table class="uninstall-nodes">
      <thead><tr><th>${escapeHtml(t('col_node'))}</th><th>${escapeHtml(t('col_scope'))}</th><th>${escapeHtml(t('col_backup'))}</th><th>${escapeHtml(t('col_state'))}</th></tr></thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function renderPlan(plan) {
  const entries = Array.isArray(plan.entries) ? plan.entries : [];
  const dependents = Array.isArray(plan.dependents) ? plan.dependents : [];
  const total = entries.filter((e) => e.removed && !e.blocks).reduce((sum, e) => sum + Number(e.sizeBytes || 0), 0);
  const nodes = fleetNodes(plan);
  return `
    <div class="uninstall-intro">${t(nodes.length ? 'intro_fleet' : 'intro', { name: `<b>${escapeHtml(plan.displayName)}</b>` })}</div>
    ${renderDependents(dependents)}
    ${renderNodeTable(nodes)}
    <div class="uninstall-blocked" data-role="blocked" hidden></div>
    ${nodes.length ? `<div class="uninstall-section-title">${escapeHtml(t('this_node_detail'))}</div>` : ''}
    ${renderEntries(entries, true)}
    ${renderEntries(entries, false)}
    ${total > 0 ? `<div class="uninstall-total">${escapeHtml(t('total', { size: formatBytes(total) }))}</div>` : ''}
    ${!nodes.length && plan.privilege === 'password' ? `<div class="uninstall-single-password">${passwordFieldHtml('uninstall-password-local', (plan.nodes || []).find((n) => n.local) || { name: '' })}</div>` : ''}
    <label class="uninstall-retype">
      <span>${t('retype', { name: `<code>${escapeHtml(plan.displayName)}</code>` })}</span>
      <tf-input id="uninstall-retype" autocomplete="off" spellcheck="false" placeholder="${escapeAttr(plan.displayName)}"></tf-input>
    </label>`;
}

/**
 * Opens the dialog for `addonId`. `displayName` seeds the title while the plan
 * loads; the plan's own name is what the admin has to retype. `onDone()` runs
 * after a successful uninstall (the caller refreshes its view).
 */
export function openUninstallDialog({ addonId, displayName, onDone }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', t('confirm_title'));
  win.setAttribute('subtitle', displayName || t('subtitle_unnamed'));
  win.setAttribute('icon', 'trash');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '460');
  win.setAttribute('width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.classList.add('addon-uninstall-window');

  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `<div class="uninstall-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.innerHTML = `
    <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    <tf-button variant="danger" icon="trash" data-action="confirm" disabled>${escapeHtml(t('button'))}</tf-button>
  `;
  win.appendChild(body);
  win.appendChild(foot);
  const confirmBtn = foot.querySelector('[data-action="confirm"]');
  const cancelBtn = foot.querySelector('[data-action="cancel"]');

  let plan = null;
  let busy = false;
  let started = false;
  // The removal never left this node: it refused before anything replicated.
  let notStarted = false;
  // The uninstall request left this browser (the teardowns then consume the
  // passwords handed out).
  let uninstallSent = false;
  // The admin closed the window: a flow still arming stops there.
  let closed = false;
  let nodes = [];
  // Per node: its own plan and where the request for it stands
  // ('loading' | 'ok' | 'offline' | 'error'), and, once started, where the
  // uninstall stands on it.
  const nodeState = new Map();
  let followUntil = 0;
  let pollTimer = null;

  const stateOfNode = (node) => nodeState.get(node.nodeId) || {};

  // A node with work, as this dialog knows it: `nodeHasWork`, and not a node
  // that answered that the instance is not installed there (critic wave 9b,
  // MINOR 10) — such a node neither counts nor holds anything back. The node
  // running the uninstall asks that peer again itself before it proceeds
  // (`teardown_preflight`, wave 11 round 2): this screen's reading is never
  // taken on trust. Should that check fail (the peer dropped off meanwhile),
  // the uninstall is refused and the peer's retype field is there.
  const hasWork = (node) => nodeHasWork(node) && stateOfNode(node).planState !== 'absent';

  // The nodes "Odinstaluj na N węzłach" counts: with work, and known to have
  // the instance — they reported a status for it, or their own plan came
  // back. A peer that never reported and has not answered yet is listed, not
  // counted.
  const countedNodes = () => nodes.filter((n) => hasWork(n) && (n.status !== 'unknown' || stateOfNode(n).planState === 'ok'));

  // Whether an OFFLINE node may be torn down when it returns: only when the
  // blockers it last published are known and empty.
  const offlineCleared = (node) => node.lastKnown === true && !(node.lastBlocks || []).length;

  // What keeps the confirm locked, as `{ node, text }` lines for the admin.
  function lockReasons() {
    if (!plan) return [{ node: null, text: '' }];
    if (!nodes.length) {
      return (plan.entries || []).filter((e) => e.blocks).map((e) => ({ node: null, text: t('blocked_reason_local', { what: entryLabel(e) }) }));
    }
    const out = [];
    for (const node of nodes.filter(hasWork)) {
      const s = stateOfNode(node);
      const name = nodeName(node);
      // Acknowledged by its retyped name: the uninstall proceeds without it.
      if (!node.local && acknowledged(node)) continue;
      if (s.planState === 'ok') {
        for (const e of (s.plan?.entries || []).filter((x) => x.blocks)) out.push({ node, text: t('blocked_reason', { node: name, what: entryLabel(e) }) });
      } else if (s.planState === 'offline') {
        if (!offlineCleared(node)) {
          const last = (node.lastBlocks || [])[0];
          out.push({ node, text: last ? t('blocked_reason', { node: name, what: entryLabel(last) }) : t('lock_unknown', { node: name }) });
        }
      } else if (s.planState === 'error') {
        out.push({ node, text: t('lock_unknown', { node: name }) });
      } else {
        out.push({ node, text: t('lock_loading', { node: name }) });
      }
    }
    return out;
  }

  // A peer that would hold the uninstall back can be passed by retyping its
  // name only when it cannot answer for itself: offline (unknown, or its
  // last published plan blocks) or its plan request failed. A CONNECTED peer
  // that refuses now is resolved on that node — its arrays dissolved or
  // moved — never acknowledged: its teardown would refuse for certain
  // (wave-9b critic, round 3, MAJOR C; the node refuses it too).
  function ackable(node) {
    if (node.local || started) return false;
    const s = stateOfNode(node);
    // Kept for a peer that said it has no instance: optional, but the way
    // on should the node running the uninstall be unable to confirm it.
    if (s.planState === 'absent') return nodeHasWork(node);
    if (!hasWork(node)) return false;
    if (s.planState === 'offline') return !offlineCleared(node);
    return s.planState === 'error';
  }
  function acknowledged(node) {
    if (!ackable(node)) return false;
    const typed = (rowOf(node.nodeId)?.querySelector('[data-role="ack-input"]')?.value || '').trim();
    return typed === ackWord(node);
  }

  const armed = () => {
    const typed = (body.querySelector('#uninstall-retype')?.value || '').trim();
    return !!plan && typed === plan.displayName && lockReasons().length === 0;
  };
  const syncButton = () => {
    if (armed() && !busy && !started) confirmBtn.removeAttribute('disabled');
    else confirmBtn.setAttribute('disabled', '');
    if (nodes.length) setText(confirmBtn, t('button_fleet', { n: countedNodes().length }));
  };

  // ----- the per-node rows, patched in place --------------------------------

  const rowOf = (nodeId) => [...body.querySelectorAll('tr[data-node]')].find((tr) => tr.dataset.node === nodeId) || null;

  function scopeLines(node) {
    const s = stateOfNode(node);
    if (node.unpaired) return [{ text: t('node_unpaired') }];
    if (s.planState === 'absent') return [{ text: t('node_absent') }];
    if (!nodeHasWork(node)) return [{ text: t('node_nothing') }];
    if (s.planState === 'offline') {
      if (offlineCleared(node)) return [{ text: t('node_offline_scope') }];
      const last = (node.lastBlocks || [])[0];
      return last
        ? [{ text: t('node_blocked', { what: entryLabel(last) }), blocked: true }]
        : [{ text: t('node_offline_unknown'), blocked: true }];
    }
    if (s.planState === 'error') return [{ text: t('node_plan_error'), blocked: true }];
    if (s.planState !== 'ok') return [{ text: I18n.t('common.loading') }];
    const lines = (s.plan.entries || []).filter((e) => e.blocks).map((e) => ({ text: t('node_blocked_live', { what: entryLabel(e) }), blocked: true }));
    for (const e of (s.plan.entries || []).filter((x) => x.removed && !x.blocks)) lines.push({ text: entryLabel(e) });
    return lines.length ? lines : [{ text: t('node_nothing') }];
  }

  function paintScope(node, row) {
    const host = row.querySelector('[data-role="scope"]');
    const lines = scopeLines(node);
    // One element per line, reused across repaints; only a changed text or
    // class is written.
    while (host.children.length > lines.length) host.lastElementChild.remove();
    lines.forEach((line, i) => {
      let el = host.children[i];
      if (!el) { el = document.createElement('span'); host.appendChild(el); }
      setText(el, line.text);
      el.classList.toggle('blocked', !!line.blocked);
    });
    const s = stateOfNode(node);
    const retry = !started && hasWork(node)
      && (s.planState === 'error' || (s.planState === 'offline' && !offlineCleared(node)));
    setAttr(row.querySelector('[data-act="retry"]'), 'hidden', !retry);
    setAttr(row.querySelector('[data-role="ack"]'), 'hidden', !ackable(node));
  }

  function nodeChip(node) {
    if (node.local) return { status: 'accent', label: t('node_local') };
    if (node.unpaired) return { status: 'neutral', label: t('node_unpaired_chip') };
    if (!node.online || stateOfNode(node).planState === 'offline') return { status: 'offline', label: t('node_offline') };
    if (node.status === 'unsupported') return { status: 'neutral', label: t('node_unsupported') };
    if (node.status === 'init_error') return { status: 'warn', label: t('node_init_error') };
    return { status: 'online', label: t('node_online') };
  }

  // n18a: "tryb A" (the helper, unattended) or "tryb B" (a password, asked
  // here). Known only from the node's own plan.
  function paintMode(node, row) {
    const own = stateOfNode(node).plan;
    const chip = row.querySelector('[data-role="mode-chip"]');
    const privilege = stateOfNode(node).planState === 'ok' ? String(own?.privilege || '') : '';
    if (privilege === 'helper' || privilege === 'password') {
      setAttr(chip, 'status', privilege === 'helper' ? 'ok' : 'warn');
      setAttr(chip, 'label', t(privilege === 'helper' ? 'mode_helper' : 'mode_password'));
      setAttr(chip, 'hidden', null);
    } else {
      setAttr(chip, 'hidden', true);
    }
    const slot = row.querySelector('[data-role="password-slot"]');
    const wants = privilege === 'password' && !started;
    if (wants && !slot.firstElementChild) {
      slot.innerHTML = passwordFieldHtml(`uninstall-password-${node.nodeId.slice(0, 16)}`, node);
    } else if (!wants && slot.firstElementChild && !started) {
      slot.innerHTML = '';
    }
    setAttr(slot, 'hidden', started);
    setText(slot.querySelector('[data-role="password-error"]'), stateOfNode(node).armError || '');
  }

  function paintBackup(node, row) {
    const own = stateOfNode(node).planState === 'ok' ? stateOfNode(node).plan : null;
    const chip = row.querySelector('[data-role="backup-chip"]');
    const file = own ? String(own.backupFile || '') : '';
    if (own && hasWork(node) && (file || own.privilege)) {
      setAttr(chip, 'status', 'ok');
      setAttr(chip, 'label', t('backup_auto'));
      setAttr(chip, 'hidden', null);
    } else {
      setAttr(chip, 'hidden', true);
    }
    setText(row.querySelector('[data-role="backup-file"]'), file);
  }

  // What the "state" cell says. Before the confirm it is a dash; after it,
  // the node's own answer, worded — a phase code, a warning code — or, for a
  // node the mesh cannot reach, that the uninstall runs when it is back.
  function stateOf(node) {
    const s = stateOfNode(node);
    if (!started) return { status: 'neutral', label: t('state.planned'), detail: '' };
    if (!hasWork(node)) return { status: 'neutral', label: t('state.nothing'), detail: '' };
    if (notStarted && !node.local) return { status: 'neutral', label: t('state.not_started'), detail: t('state.not_started_detail') };
    if (s.unreachable) return { status: 'offline', label: t('state.unreachable'), detail: t('state.unreachable_detail') };
    const st = s.status;
    if (!st) return { status: 'info', label: t('state.waiting'), detail: '' };
    const warnings = (st.warnings || []).map((w) => worded('warnings', w)).filter(Boolean).join(' · ');
    switch (st.state) {
      case 'installed':
        return notStarted
          ? { status: 'err', label: t('state.refused'), detail: s.refusal || '' }
          : { status: 'info', label: t('state.waiting'), detail: t('state.waiting_detail') };
      case 'running': return { status: 'accent', label: t('state.running'), detail: worded('phases', st.phase) };
      case 'done': return { status: warnings ? 'warn' : 'ok', label: t(warnings ? 'state.done_with_warnings' : 'state.done'), detail: warnings };
      case 'failed': {
        // What refused, from the node's own plan read after the failure
        // (critic wave 9b, MINOR 8): "3 macierze Elastic pod nadzorem", not
        // only the step it stopped at.
        // A peer's instance row is gone once the removal reached it, so its
        // plan cannot be read again: a refusal in the check step is then
        // the blocker it had shown or last published.
        // Only a failure IN a refusal step is a refusal (wave 11 round 2):
        // a failed wipe or backup on a node that also has a blocker says
        // the step it failed at.
        const refusal = REFUSAL_PHASES.has(st.phase);
        const known = !refusal ? []
          : s.failBlocks?.length ? s.failBlocks
            : [...(s.plan?.entries || []).filter((e) => e.blocks), ...(node.lastBlocks || [])].slice(0, 1);
        const blocks = known.map(entryLabel).filter(Boolean);
        if (blocks.length) return { status: 'err', label: t('state.failed'), detail: t('state.failed_because', { what: blocks.join(', ') }) };
        const phase = worded('phases', st.phase);
        return { status: 'err', label: t('state.failed'), detail: phase ? t('state.failed_in', { phase }) : '' };
      }
      case 'absent': return { status: 'ok', label: t('state.absent'), detail: '' };
      default: return { status: 'neutral', label: t('state.waiting'), detail: '' };
    }
  }

  function paintNode(node) {
    const row = rowOf(node.nodeId);
    if (!row) return;
    const chip = nodeChip(node);
    const nodeChipEl = row.querySelector('[data-role="node-chip"]');
    setAttr(nodeChipEl, 'status', chip.status);
    setAttr(nodeChipEl, 'label', chip.label);
    paintScope(node, row);
    paintMode(node, row);
    paintBackup(node, row);
    const state = stateOf(node);
    const stateChip = row.querySelector('[data-role="state-chip"]');
    setAttr(stateChip, 'status', state.status);
    setAttr(stateChip, 'label', state.label);
    setText(row.querySelector('[data-role="state-detail"]'), state.detail);
  }

  function paintBlocked() {
    const el = body.querySelector('[data-role="blocked"]');
    if (!el) return;
    const text = started ? '' : lockReasons().map((r) => r.text).filter(Boolean).join(' · ');
    setText(el, text);
    el.hidden = !text;
  }

  const paintAll = () => { nodes.forEach(paintNode); paintBlocked(); syncButton(); };

  // One node's own plan: what the uninstall does THERE, whether that node
  // refuses it, its mode and its backup. Asked on the node itself
  // (forwarded). A node the mesh cannot reach is 'offline' and judged by what
  // it last published; any other failure leaves its plan unknown.
  function loadPlan(node) {
    if (node.local) { nodeState.set(node.nodeId, { plan, planState: 'ok' }); return; }
    if (!nodeHasWork(node)) { nodeState.set(node.nodeId, { planState: 'ok', plan: { entries: [] } }); return; }
    if (!node.online) { nodeState.set(node.nodeId, { planState: 'offline' }); return; }
    nodeState.set(node.nodeId, { planState: 'loading' });
    ApiBinary.action('addonTeardownPlanRequest', { addonId }, { targetNodeId: node.nodeId }).then((own) => {
      nodeState.set(node.nodeId, { ...stateOfNode(node), plan: own, planState: 'ok' });
      paintAll();
    }, (err) => {
      // NotFound is the node itself answering that the instance is not
      // installed there: nothing to do on it (MINOR 10).
      const planState = err?.code === 'NodeUnreachable' ? 'offline' : err?.code === 'NotFound' ? 'absent' : 'error';
      nodeState.set(node.nodeId, { ...stateOfNode(node), planState });
      paintAll();
    });
  }

  body.addEventListener('input', (e) => {
    if (e.target.closest('[data-role="ack-input"]')) paintAll();
  });

  body.addEventListener('click', (e) => {
    const retry = e.target.closest('[data-act="retry"]');
    if (!retry || started) return;
    const node = nodes.find((n) => n.nodeId === retry.closest('tr[data-node]')?.dataset.node);
    if (!node) return;
    // A retry asks even a node the roster calls offline: it may be back.
    loadPlan({ ...node, online: true });
    paintAll();
  });

  // ----- after the confirm ---------------------------------------------------

  // Every mode-B node whose admin typed its password: armed right before the
  // uninstall, on that node. A rejected password stops everything — nothing
  // is removed anywhere — and is said on that node's row.
  // The nodes this dialog handed a teardown password (MAJOR B, round 3). Each
  // one is disarmed whenever the flow stops before the uninstall went out —
  // the teardowns consume them otherwise.
  const armedNodes = [];
  async function disarmAll() {
    const nodesToDisarm = armedNodes.splice(0);
    await Promise.all(nodesToDisarm.map((node) => ApiBinary.action('addonTeardownDisarmRequest', { addonId },
      node && !node.local ? { targetNodeId: node.nodeId } : undefined).catch(() => {})));
  }

  async function armPasswords() {
    const jobs = [];
    const single = body.querySelector('#uninstall-password-local');
    if (single && single.value) jobs.push({ node: null, password: single.value });
    for (const node of nodes) {
      const input = rowOf(node.nodeId)?.querySelector('[data-role="password"]');
      if (input && input.value) jobs.push({ node, password: input.value });
    }
    let ok = true;
    for (const job of jobs) {
      // Closed mid-arming (critic round 3, C6): no further node is armed;
      // the caller takes back what was already handed out.
      if (closed) break;
      try {
        await ApiBinary.action('addonTeardownArmRequest', { addonId, sudoPassword: job.password },
          job.node && !job.node.local ? { targetNodeId: job.node.nodeId } : undefined);
        armedNodes.push(job.node);
        if (job.node) nodeState.set(job.node.nodeId, { ...stateOfNode(job.node), armError: '' });
      } catch (err) {
        ok = false;
        const words = /teardown_password_rejected/.test(String(err?.message || '')) ? t('password_rejected') : t('password_unreachable');
        if (job.node) nodeState.set(job.node.nodeId, { ...stateOfNode(job.node), armError: words });
        else setText(body.querySelector('.uninstall-single-password [data-role="password-error"]'), words);
      }
    }
    // The passwords are not kept on screen past this point.
    body.querySelectorAll('[data-role="password"]').forEach((input) => { input.value = ''; });
    return ok;
  }

  async function pollStatuses() {
    pollTimer = null;
    if (!win.isConnected) return;
    // After a refusal nothing reached the other nodes: only this one is read.
    const open = nodes.filter((n) => hasWork(n) && (!notStarted || n.local) && !FINAL_STATES.has(stateOfNode(n).status?.state));
    await Promise.all(open.map(async (node) => {
      try {
        const status = await ApiBinary.action('addonTeardownStatusRequest', { addonId }, node.local ? undefined : { targetNodeId: node.nodeId });
        nodeState.set(node.nodeId, { ...stateOfNode(node), status, unreachable: false });
        // A failed teardown keeps the instance on its node: its plan, read
        // now, says what refused (MINOR 8). Asked once; a failed read leaves
        // the step it stopped at.
        if (status?.state === 'failed' && !stateOfNode(node).failAsked) {
          nodeState.set(node.nodeId, { ...stateOfNode(node), failAsked: true });
          try {
            const own = await ApiBinary.action('addonTeardownPlanRequest', { addonId }, node.local ? undefined : { targetNodeId: node.nodeId });
            nodeState.set(node.nodeId, { ...stateOfNode(node), failBlocks: (own?.entries || []).filter((e) => e.blocks) });
          } catch { /* the step it stopped at is what is known */ }
        }
      } catch (err) {
        if (err?.code === 'NodeUnreachable' || !node.online) {
          nodeState.set(node.nodeId, { ...stateOfNode(node), unreachable: true });
        }
      }
      paintNode(node);
    }));
    const pending = nodes.some((n) => hasWork(n) && !FINAL_STATES.has(stateOfNode(n).status?.state));
    if (pending && !notStarted && Date.now() < followUntil && win.isConnected) pollTimer = setTimeout(pollStatuses, STATUS_POLL_MS);
  }

  function enterProgress() {
    started = true;
    followUntil = Date.now() + STATUS_FOLLOW_MS;
    body.querySelector('.uninstall-retype')?.setAttribute('hidden', '');
    confirmBtn.setAttribute('hidden', '');
    setText(cancelBtn, I18n.t('common.close'));
    paintAll();
  }

  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') {
      closed = true;
      if (pollTimer) clearTimeout(pollTimer);
      if (!uninstallSent) void disarmAll();
      win.close(true);
      return;
    }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (!armed() || busy || started) return;
    busy = true;
    syncButton();
    // Read before anything moves the rows on: which peers the admin chose to
    // proceed without, each with its name retyped.
    const acknowledgedNodes = nodes.filter((n) => acknowledged(n)).map((n) => ({ nodeId: n.nodeId, confirmName: ackWord(n) }));
    const armedOk = await armPasswords();
    if (closed) {
      // Closed while the passwords were being handed out: nothing is
      // uninstalled, and what was handed out is taken back.
      await disarmAll();
      return;
    }
    if (!armedOk) {
      // Another node refused its password: the ones already armed are
      // disarmed, nothing is removed anywhere.
      await disarmAll();
      busy = false;
      paintAll();
      toast(t('password_stop'), 'error');
      return;
    }
    const fleet = nodes.length > 0;
    if (fleet) {
      enterProgress();
      // This node's row moves while the request runs; the others wait for the
      // replicated removal to reach them.
      pollTimer = setTimeout(pollStatuses, STATUS_POLL_MS);
    }
    try {
      uninstallSent = true;
      const res = await ApiBinary.action('addonUninstallRequest', { addonId, acknowledgedNodes });
      if (res && res.ok === false) throw new Error(t('error'));
      toast(t('success', { name: plan.displayName }), 'success');
      if (!fleet) {
        win.close(true);
        await onDone?.();
        return;
      }
      await onDone?.();
      if (pollTimer) clearTimeout(pollTimer);
      await pollStatuses();
    } catch (err) {
      busy = false;
      const code = NOT_STARTED.exec(String(err?.message || ''))?.[1];
      const refusal = code ? worded('refusal', code) || t('error') : '';
      // Taken back ONLY when it is certain nothing went out: the node refused
      // before replicating (its `refusal:` code). Any other failure — also
      // one with no code, e.g. a deadline — may have come after the removal
      // replicated, and a disarm would then rob the peers' teardowns of the
      // password they need (critic round 3, C4). An unused password expires
      // on its node. (The node drops its own on a refusal.)
      if (code) {
        uninstallSent = false;
        await disarmAll();
      }
      if (!fleet) {
        syncButton();
        // Worded by class: the node's own text can carry a path with the
        // instance id (critic R1); it stays in the node's log.
        toast(refusal || t('error_see_log'), 'error');
        return;
      }
      if (pollTimer) clearTimeout(pollTimer);
      if (code) {
        // The node refused BEFORE the removal replicated
        // (`teardown_preflight`): nothing reached any other node, so their
        // rows say it never started and nothing is followed.
        notStarted = true;
        const local = nodes.find((n) => n.local);
        if (local) nodeState.set(local.nodeId, { ...stateOfNode(local), refusal });
        toast(refusal, 'error');
        paintAll();
        await pollStatuses();
        return;
      }
      // Any other failure happened AFTER the removal was replicated (the
      // tombstone is written before this node's own teardown runs,
      // `addon_uninstall`): the other nodes tear down on their own, so their
      // rows are still followed; this node's record says in which step it
      // failed.
      toast(t('error'), 'error');
      await pollStatuses();
    }
  });
  win.addEventListener('close-request', () => {
    closed = true;
    if (pollTimer) clearTimeout(pollTimer);
    if (!uninstallSent) void disarmAll();
  });

  document.body.appendChild(win);

  ApiBinary.one('addonTeardownPlanRequest', { addonId }).then((res) => {
    plan = res;
    nodes = fleetNodes(plan);
    win.setAttribute('subtitle', plan.displayName || displayName || t('subtitle_unnamed'));
    if (nodes.length) win.setAttribute('width', '820');
    // Last: tf-window consumes `title` into its header and re-reads it on
    // every later attribute change, so a title set before them would be lost.
    win.setAttribute('title', nodes.length ? t('confirm_title_fleet', { name: plan.displayName }) : t('confirm_title'));
    body.innerHTML = renderPlan(plan);
    body.querySelector('#uninstall-retype')?.addEventListener('input', syncButton);
    nodes.forEach(loadPlan);
    paintAll();
  }).catch((err) => {
    body.innerHTML = `<div class="alert warn"><svg class="icon"><use href="#i-alert"/></svg><div>${escapeHtml(t('plan_error'))}: ${escapeHtml(err.message)}</div></div>`;
  });

  return win;
}
