// =============================================================================
// File: modules/addons/disable-dialog.js
// Description: The confirmation before an addon instance is DISABLED (n18d).
//              Generic platform code: it asks the answering node what
//              disabling does there (AddonDisablePreviewRequest) — an app
//              supplies those consequences from its own real state through
//              its `disable_consequences` provider (TentaNas: shares and
//              targets that keep serving, schedules that stop or go on,
//              arrays and pools that stay) — and words each one. An app with
//              no provider and no background work is disabled without a
//              dialog, exactly as before.
//
//              Disabling is FLEET-WIDE, so an instance that lives on several
//              nodes gets one row per node (wave 10, in the style of the
//              uninstall table): the node's real name, its mode chip (A/B)
//              and ITS OWN consequences, asked of that node (the preview
//              forwarded to it) — loading, offline and failed each worded,
//              never an id.
//
//              TentaNas also offers n18d's second action, "Wyłącz i zatrzymaj
//              udostępnianie…": a four-eyes request for THIS node
//              (`tentaNasSharingStopRequest`). Nothing is disabled when it is
//              sent — the request parks, a second admin releases it, and only
//              then does the node take its shares and targets out of service
//              and disable TentaNas. The switch therefore stays on.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { setText, setAttr } from '/js/lib/dom-patch.js';
import { approvalDetail } from '/js/modules/tentanas/approvals.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';

const t = (key, vars) => I18n.t(`addon_disable.${key}`, vars);
const tu = (key, vars) => I18n.t(`addon_uninstall.${key}`, vars);

// What each effect looks like: the chip's status and the row's icon. Every
// status is one tf-chip knows (STATUS_CLASSES), so none renders as the
// neutral default by accident.
const EFFECT = {
  continues: { status: 'ok', icon: 'play' },
  stops: { status: 'warn', icon: 'pause' },
  kept: { status: 'info', icon: 'check' },
};

/** The packages that offer "stop sharing on this node" with the disable. */
const STOP_SHARING_PACKAGES = new Set(['tentanas']);

/** The refusals the stop request can answer, worded here. */
const STOP_REFUSAL = /refusal:(sharing_stop_[a-z_]+)/;

/** One consequence as a sentence in the reader's language; the node's `kind` never shows. */
export function consequenceText(c) {
  const key = `addon_disable.consequences.${c.kind}`;
  const vars = { ...(c.countVars || {}), names: (Array.isArray(c.names) ? c.names : []).join(', ') };
  const words = I18n.t(key, vars);
  return words === key ? '' : words;
}

/** The worded consequences of one preview, in the node's order. */
function wordedConsequences(preview) {
  return (preview?.consequences || [])
    .map((c) => ({ c, text: consequenceText(c) }))
    .filter((r) => r.text);
}

function consequenceItemHtml({ c, text }) {
  const effect = EFFECT[c.effect] || EFFECT.kept;
  return `<li class="disable-consequence ${escapeHtml(EFFECT[c.effect] ? c.effect : 'kept')}">
    <svg class="icon"><use href="#i-${effect.icon}"/></svg>
    <span class="disable-consequence-text">${escapeHtml(text)}</span>
    <tf-chip size="sm" status="${effect.status}" label="${escapeHtml(t('effect_' + (EFFECT[c.effect] ? c.effect : 'kept')))}"></tf-chip>
  </li>`;
}

function consequencesHtml(preview) {
  const rows = wordedConsequences(preview);
  if (!rows.length) return '';
  const where = preview.nodeName ? t('on_node', { node: preview.nodeName }) : t('on_this_node');
  return `
    <div class="disable-consequences">
      <div class="disable-consequences-title">${escapeHtml(where)}</div>
      <ul class="disable-consequence-list">${rows.map(consequenceItemHtml).join('')}</ul>
    </div>`;
}

/** A node as the screen names it: its name, or words — never its id. */
function nodeName(node) {
  const name = String(node?.name || '').trim();
  return name || tu('node_unnamed');
}

/** The nodes the disable reaches; an older node's preview has none. */
function fleetNodes(preview) {
  return Array.isArray(preview?.nodes) && preview.nodes.length > 1 ? preview.nodes : [];
}

function nodeTableHtml(nodes) {
  const rows = nodes.map((n) => `
    <tr data-node="${escapeAttr(n.nodeId)}">
      <td>
        <div class="uninstall-node-name">${escapeHtml(nodeName(n))}</div>
        <div class="uninstall-node-meta"><tf-chip size="sm" data-role="node-chip"></tf-chip> <tf-chip size="sm" data-role="mode-chip" hidden></tf-chip></div>
      </td>
      <td>
        <ul class="disable-consequence-list" data-role="effects"></ul>
        <div class="disable-node-note" data-role="note"></div>
        <tf-button size="sm" variant="ghost" icon="refresh" data-act="retry" hidden>${escapeHtml(tu('retry'))}</tf-button>
      </td>
    </tr>`).join('');
  return `
    <table class="uninstall-nodes disable-nodes">
      <thead><tr><th>${escapeHtml(tu('col_node'))}</th><th>${escapeHtml(t('col_effects'))}</th></tr></thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function bodyHtml(preview, error, nodes, stopSharing) {
  return `
    <div class="disable-intro">
      <b>${escapeHtml(t('not_uninstall'))}</b> ${escapeHtml(t('intro'))}
      ${preview?.backgroundOnDisable ? `<div class="disable-background"><tf-chip size="sm" status="info" label="${escapeHtml(t('background_label'))}"></tf-chip> <span>${escapeHtml(t('background_hint'))}</span></div>` : ''}
    </div>
    ${error ? `<div class="alert warn"><svg class="icon"><use href="#i-alert"/></svg><div>${escapeHtml(t('preview_error', { error }))}</div></div>` : ''}
    ${nodes.length ? nodeTableHtml(nodes) : (preview ? consequencesHtml(preview) : '')}
    ${stopSharing ? `<div class="alert warn disable-stop-hint" data-role="stop-hint"><svg class="icon"><use href="#i-alert"/></svg><div>${escapeHtml(t('stop_hint'))}</div></div>` : ''}
    <div class="disable-stop" data-role="stop-panel" hidden>
      <div class="disable-stop-title" data-role="stop-title"></div>
      <div class="disable-stop-text" data-role="stop-text"></div>
      <ul class="disable-stop-list" data-role="stop-list"></ul>
      <div class="disable-stop-text" data-role="stop-approval"></div>
      <div class="disable-stop-result" data-role="stop-result" hidden></div>
    </div>`;
}

/** How many shares/targets of each protocol serve on a node, from its preview. */
function servingCounts(preview) {
  const out = { smb: 0, nfs: 0, iscsi: 0, nvmet: 0 };
  for (const c of preview?.consequences || []) {
    const n = Number(c.countVars?.n) || 0;
    if (c.kind === 'tentanas_smb_shares_continue') out.smb = n;
    else if (c.kind === 'tentanas_nfs_shares_continue') out.nfs = n;
    else if (/^tentanas_iscsi_targets_/.test(c.kind)) out.iscsi = n;
    else if (/^tentanas_nvmet_targets_/.test(c.kind)) out.nvmet = n;
  }
  return out;
}

/**
 * Resolves true when the admin confirmed the disable (or when there is
 * nothing to confirm: the app has no consequence provider and no background
 * work), false when the dialog was dismissed — and false after a stop-sharing
 * request was parked: nothing is disabled until a second admin releases it.
 */
export async function confirmDisable({ addonId, displayName, packageId = '' }) {
  let preview = null;
  let error = '';
  try {
    preview = await ApiBinary.one('addonDisablePreviewRequest', { addonId });
  } catch (err) {
    error = err?.message || String(err);
  }
  if (preview && !(preview.consequences || []).length && !preview.backgroundOnDisable) return true;

  const name = preview?.displayName || displayName || '';
  const nodes = fleetNodes(preview);
  const stopSharing = STOP_SHARING_PACKAGES.has(String(packageId || '')) && !!preview;
  return new Promise((resolve) => {
    const win = document.createElement('tf-window');
    win.setAttribute('buttons', 'close');
    win.setAttribute('icon', 'pause');
    win.setAttribute('draggable', '');
    win.setAttribute('min-width', '460');
    win.setAttribute('width', nodes.length ? '760' : '560');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    // Last: tf-window consumes `title` into its header and re-reads it on
    // every later attribute change.
    win.setAttribute('title', t('title', { name }));
    win.classList.add('addon-disable-window');
    const body = document.createElement('div');
    body.slot = 'body';
    body.innerHTML = bodyHtml(preview, error, nodes, stopSharing);
    const foot = document.createElement('div');
    foot.slot = 'footer';
    foot.innerHTML = `
      <tf-button variant="secondary" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="secondary" icon="chevron-left" data-action="stop-back" hidden>${escapeHtml(I18n.t('common.back'))}</tf-button>
      ${stopSharing ? `<tf-button variant="danger" icon="stop" data-action="stop">${escapeHtml(t('stop_button'))}</tf-button>` : ''}
      <tf-button variant="danger" icon="shield" data-action="stop-send" hidden>${escapeHtml(t('stop_send'))}</tf-button>
      <tf-button variant="primary" icon="pause" data-action="confirm">${escapeHtml(t(stopSharing ? 'confirm_keep_serving' : 'confirm'))}</tf-button>`;
    win.appendChild(body);
    win.appendChild(foot);
    const button = (action) => foot.querySelector(`[data-action="${action}"]`);

    // ----- the per-node rows, patched in place ---------------------------------

    // Per node: its own preview and where the request for it stands
    // ('loading' | 'ok' | 'offline' | 'error').
    const nodeState = new Map();
    const stateOf = (node) => nodeState.get(node.nodeId) || {};
    const rowOf = (nodeId) => [...body.querySelectorAll('tr[data-node]')].find((tr) => tr.dataset.node === nodeId) || null;

    function nodeChip(node) {
      if (node.local) return { status: 'accent', label: tu('node_local') };
      if (node.unpaired) return { status: 'neutral', label: tu('node_unpaired_chip') };
      if (!node.online || stateOf(node).state === 'offline') return { status: 'offline', label: tu('node_offline') };
      if (node.status === 'unsupported') return { status: 'neutral', label: tu('node_unsupported') };
      if (node.status === 'init_error') return { status: 'warn', label: tu('node_init_error') };
      return { status: 'online', label: tu('node_online') };
    }

    // What the row says besides its consequences.
    function noteOf(node) {
      const s = stateOf(node);
      if (node.unpaired) return t('node_unpaired');
      if (node.status === 'unsupported') return t('node_unsupported');
      if (s.state === 'offline') return t('node_offline');
      if (s.state === 'error') return t('node_error');
      if (s.state !== 'ok') return I18n.t('common.loading');
      return wordedConsequences(s.preview).length ? '' : t('node_nothing');
    }

    // One element per consequence, reused across repaints: only a changed
    // text, class or attribute is written.
    function paintEffects(host, rows) {
      while (host.children.length > rows.length) host.lastElementChild.remove();
      rows.forEach(({ c }, i) => {
        let li = host.children[i];
        if (!li) {
          host.insertAdjacentHTML('beforeend', consequenceItemHtml(rows[i]));
          return;
        }
        const effect = EFFECT[c.effect] ? c.effect : 'kept';
        for (const k of Object.keys(EFFECT)) li.classList.toggle(k, k === effect);
        setAttr(li.querySelector('use'), 'href', `#i-${EFFECT[effect].icon}`);
        setText(li.querySelector('.disable-consequence-text'), rows[i].text);
        const chip = li.querySelector('tf-chip');
        setAttr(chip, 'status', EFFECT[effect].status);
        setAttr(chip, 'label', t('effect_' + effect));
      });
    }

    function paintNode(node) {
      const row = rowOf(node.nodeId);
      if (!row) return;
      const s = stateOf(node);
      const chip = nodeChip(node);
      const nodeChipEl = row.querySelector('[data-role="node-chip"]');
      setAttr(nodeChipEl, 'status', chip.status);
      setAttr(nodeChipEl, 'label', chip.label);
      // n18a/n18d: "tryb A" (the helper, unattended) or "tryb B" (a
      // password) — known only from the node's own preview.
      const privilege = s.state === 'ok' ? String(s.preview?.privilege || '') : '';
      const mode = row.querySelector('[data-role="mode-chip"]');
      if (privilege === 'helper' || privilege === 'password') {
        setAttr(mode, 'status', privilege === 'helper' ? 'ok' : 'warn');
        setAttr(mode, 'label', tu(privilege === 'helper' ? 'mode_helper' : 'mode_password'));
        setAttr(mode, 'hidden', null);
      } else {
        setAttr(mode, 'hidden', true);
      }
      paintEffects(row.querySelector('[data-role="effects"]'), s.state === 'ok' ? wordedConsequences(s.preview) : []);
      const note = noteOf(node);
      const noteEl = row.querySelector('[data-role="note"]');
      setText(noteEl, note);
      setAttr(noteEl, 'hidden', !note);
      setAttr(row.querySelector('[data-act="retry"]'), 'hidden', !(s.state === 'error' || s.state === 'offline'));
    }

    // One node's own consequences, asked on that node (forwarded). A node
    // the mesh cannot reach is 'offline': the disable replicates to it when
    // it is back, and what it does there is not known now.
    function loadNode(node) {
      if (node.local) { nodeState.set(node.nodeId, { state: 'ok', preview }); return; }
      if (node.unpaired || node.status === 'unsupported') { nodeState.set(node.nodeId, { state: 'ok', preview: null }); return; }
      if (!node.online) { nodeState.set(node.nodeId, { state: 'offline' }); return; }
      nodeState.set(node.nodeId, { state: 'loading' });
      ApiBinary.action('addonDisablePreviewRequest', { addonId }, { targetNodeId: node.nodeId }).then((own) => {
        nodeState.set(node.nodeId, { state: 'ok', preview: own });
        paintNode(node);
      }, (err) => {
        nodeState.set(node.nodeId, { state: err?.code === 'NodeUnreachable' ? 'offline' : 'error' });
        paintNode(node);
      });
    }

    body.addEventListener('click', (e) => {
      const retry = e.target.closest('[data-act="retry"]');
      if (!retry) return;
      const node = nodes.find((n) => n.nodeId === retry.closest('tr[data-node]')?.dataset.node);
      if (!node) return;
      // A retry asks even a node the roster calls offline: it may be back.
      loadNode({ ...node, online: true });
      paintNode(node);
    });

    // ----- "Wyłącz i zatrzymaj udostępnianie…" (n18d) ---------------------------

    const localNode = nodes.find((n) => n.local) || null;
    const localName = String(localNode?.name || preview?.nodeName || '').trim();
    let stopStep = false;
    let parked = false;
    let sending = false;

    function paintStopPanel() {
      const panel = body.querySelector('[data-role="stop-panel"]');
      setAttr(panel, 'hidden', !stopStep);
      setAttr(body.querySelector('[data-role="stop-hint"]'), 'hidden', stopStep);
      const table = body.querySelector('.disable-nodes') || body.querySelector('.disable-consequences');
      setAttr(table, 'hidden', stopStep);
      setAttr(button('stop'), 'hidden', stopStep);
      setAttr(button('confirm'), 'hidden', stopStep);
      setAttr(button('stop-back'), 'hidden', !stopStep || parked);
      setAttr(button('stop-send'), 'hidden', !stopStep || parked);
      setAttr(button('stop-send'), 'disabled', sending);
      setText(button('cancel'), parked ? I18n.t('common.close') : I18n.t('common.cancel'));
      if (!stopStep) return;
      const node = localName || tu('node_unnamed');
      setText(body.querySelector('[data-role="stop-title"]'), t('stop_title', { node }));
      setText(body.querySelector('[data-role="stop-text"]'), t('stop_text'));
      const counts = servingCounts(preview);
      const items = [
        counts.smb ? t('stop_item_smb', { n: counts.smb }) : '',
        counts.nfs ? t('stop_item_nfs', { n: counts.nfs }) : '',
        counts.iscsi ? t('stop_item_iscsi', { n: counts.iscsi }) : '',
        counts.nvmet ? t('stop_item_nvmet', { n: counts.nvmet }) : '',
      ].filter(Boolean);
      if (!items.length) items.push(t('stop_item_none'));
      items.push(t('stop_item_disable'));
      const list = body.querySelector('[data-role="stop-list"]');
      while (list.children.length > items.length) list.lastElementChild.remove();
      items.forEach((text, i) => {
        let li = list.children[i];
        if (!li) { li = document.createElement('li'); list.appendChild(li); }
        setText(li, text);
      });
      setText(body.querySelector('[data-role="stop-approval"]'), t('stop_approval'));
    }

    function showParked(approval) {
      parked = true;
      const result = body.querySelector('[data-role="stop-result"]');
      const detail = approvalDetail(approval).text;
      setText(result, `${t('stop_parked')}${detail ? ' ' + detail : ''}`);
      setAttr(result, 'hidden', false);
      paintStopPanel();
    }

    async function sendStop() {
      if (sending || parked) return;
      sending = true;
      paintStopPanel();
      try {
        // The request is for THIS node: the dashboard's own, so it is not
        // forwarded anywhere.
        const res = await ApiBinary.action('tentaNasSharingStopRequest', {});
        if (!res?.approval?.requestId) throw new Error(t('stop_error'));
        toast(t('stop_parked'), 'warning');
        showParked(res.approval);
      } catch (err) {
        const code = STOP_REFUSAL.exec(String(err?.message || ''))?.[1];
        const key = code ? `addon_disable.refusal.${code}` : '';
        const words = key ? I18n.t(key) : '';
        toast(words && words !== key ? words : t('stop_error'), 'error');
      } finally {
        sending = false;
        paintStopPanel();
      }
    }

    // ----- the window ----------------------------------------------------------

    let settled = false;
    const settle = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
    };
    win.addEventListener('action', (e) => {
      const action = e.detail?.action;
      if (action === 'confirm') { settle(true); win.close(true); }
      else if (action === 'cancel') { settle(false); win.close(true); }
      else if (action === 'stop') { e.preventDefault?.(); stopStep = true; paintStopPanel(); }
      else if (action === 'stop-back') { e.preventDefault?.(); stopStep = false; paintStopPanel(); }
      else if (action === 'stop-send') { e.preventDefault?.(); void sendStop(); }
    });
    // The header's close button (and anything else that closes the window).
    win.addEventListener('close-request', () => settle(false));
    document.body.appendChild(win);
    nodes.forEach(loadNode);
    nodes.forEach(paintNode);
    paintStopPanel();
  });
}
