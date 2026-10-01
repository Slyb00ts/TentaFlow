// =============================================================================
// File: modules/org-structure/visibility-tab.js
// Description: The Widoczność tab (mockup f06): a read-only inspector of who
//   sees what, in both directions — what the chosen person sees, area by area
//   with the rule behind each answer, and who sees the chosen person's
//   personal data (docs §6.3). The answers come from the server, which
//   evaluates the same rules the data reads are gated by; nothing is derived
//   here, so the screen cannot promise what the server would refuse.
//   A member inspects themselves; only an administrator picks somebody else
//   (the server refuses anything else).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import '/js/components/tf-select.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-avatar.js';
import '/js/components/tf-alert.js';
import { initials, personOptions, viewerRows, visibilityRows } from '/js/modules/org-structure/cover-model.js';

const vt = (key, params) => I18n.t(`org_structure.visibility.${key}`, params);

const SHOWN_NAMES = 4;

let state = null;

function nameList(people) {
  const names = people.map((p) => p.display_name || '—');
  if (names.length <= SHOWN_NAMES) return names.join(', ');
  return vt('people_more', { names: names.slice(0, SHOWN_NAMES).join(', '), count: names.length });
}

function seesRowHtml(row) {
  const sub = row.people.length ? nameList(row.people) : vt(`area_${row.area}_sub`);
  const verdict = row.verdict === 'subtree' || row.verdict === 'direct'
    ? vt(`verdict_${row.verdict}`, { count: row.count }) : vt(`verdict_${row.verdict}`);
  return `
    <tr>
      <td class="org-vis-area"><b>${escapeHtml(vt(`area_${row.area}`))}</b><span class="org-vis-sub">${escapeHtml(sub)}</span></td>
      <td><span class="org-vis-verdict org-vis-${row.tone}">${escapeHtml(verdict)}</span></td>
      <td><span class="org-vis-rule${row.rule === 'none' ? ' org-vis-rule-none' : ''}">${escapeHtml(vt(`rule_${row.rule}`))}</span></td>
    </tr>`;
}

function renderSees(response) {
  const rows = visibilityRows(response);
  const name = response.user.display_name || '—';
  return `
    <div class="org-vis-card-head"><h3>${escapeHtml(vt('sees_title', { name }))}</h3><span class="org-vis-hint">${escapeHtml(vt('sees_hint'))}</span></div>
    <div class="org-vis-scroll">
      <table class="org-vis-table">
        <thead><tr>
          <th scope="col">${escapeHtml(vt('col_area'))}</th>
          <th scope="col">${escapeHtml(vt('col_verdict'))}</th>
          <th scope="col">${escapeHtml(vt('col_rule'))}</th>
        </tr></thead>
        <tbody>${rows.map(seesRowHtml).join('')}</tbody>
      </table>
    </div>`;
}

function viewerHtml(viewer) {
  const kinds = (viewer.kinds ?? []).map((k) => vt(`kind_${k}`)).join(', ');
  const note = viewer.self ? vt('viewer_self') : kinds;
  return `
    <div class="org-vis-who-row">
      <tf-avatar initials="${escapeAttr(initials(viewer.display_name))}" size="sm"></tf-avatar>
      <div class="org-vis-who-main">
        <div class="org-vis-who-name">${escapeHtml(viewer.display_name || '—')}</div>
        <div class="org-vis-who-kinds">${escapeHtml(note)}</div>
      </div>
      <span class="org-vis-rule">${escapeHtml(vt(`rule_${viewer.rule}`))}</span>
    </div>`;
}

function renderWho(response) {
  const name = response.subject.display_name || '—';
  const viewers = viewerRows(response, response.subject.user_id);
  return `
    <div class="org-vis-card-head"><h3>${escapeHtml(vt('who_title', { name }))}</h3></div>
    <div class="org-vis-who-ask">${escapeHtml(vt('who_sub'))}</div>
    <div class="org-vis-who">${viewers.map(viewerHtml).join('')}</div>
    <p class="org-vis-note">${escapeHtml(vt('who_note'))}</p>`;
}

function renderChips(response) {
  const chips = [];
  if (response.manager) chips.push(`<tf-chip variant="outline" icon="user">${escapeHtml(vt('chip_manager', { name: response.manager.display_name || '—' }))}</tf-chip>`);
  chips.push(`<tf-chip variant="outline" icon="users">${escapeHtml(vt('chip_subtree', { count: response.subtree.length }))}</tf-chip>`);
  return chips.join('');
}

function draw(sees, who) {
  const root = state?.root;
  if (!root) return;
  root.querySelector('#org-vis-chips').innerHTML = renderChips(sees);
  root.querySelector('#org-vis-sees').innerHTML = renderSees(sees);
  root.querySelector('#org-vis-who-card').innerHTML = renderWho(who);
  root.querySelector('#org-vis-error').hidden = true;
}

async function load(userId) {
  const request = userId ? { userId } : {};
  const token = ++state.token;
  try {
    const [sees, who] = await Promise.all([
      ApiBinary.one('orgVisibilityRequest', request),
      ApiBinary.one('orgWhoSeesRequest', userId ? { subjectUserId: userId } : {}),
    ]);
    if (!state || token !== state.token) return;
    draw(sees, who);
  } catch (err) {
    if (!state || token !== state.token) return;
    const alert = state.root.querySelector('#org-vis-error');
    alert.setAttribute('message', vt('load_failed', { message: err.message || '' }));
    alert.hidden = false;
  }
}

function fillPicker(view, isAdmin) {
  const select = state.root.querySelector('#org-vis-person');
  if (!select) return;
  const people = personOptions(view);
  select.setOptions([{ value: '', label: vt('pick_me') }, ...people.map((p) => ({ value: p.id, label: p.name }))], state.userId ?? '');
}

/**
 * Draws the Widoczność tab into `host`. `view` and `myPermissions` come from a structure answer.
 */
export async function mountVisibilityTab(host, { view, myPermissions = [] }) {
  unmountVisibilityTab();
  const isAdmin = myPermissions.includes('org.admin');
  const root = document.createElement('div');
  root.className = 'org-vis';
  root.innerHTML = `
    <div class="org-vis-top">
      ${isAdmin
    ? `<tf-select id="org-vis-person" class="org-vis-pick" label="${escapeAttr(vt('pick_label'))}"></tf-select>`
    : `<div class="org-vis-self">${escapeHtml(vt('self_only'))}</div>`}
      <div class="org-vis-chips" id="org-vis-chips"></div>
    </div>
    <tf-alert id="org-vis-error" tone="danger" role="alert" hidden></tf-alert>
    <div class="org-vis-grid">
      <section class="org-vis-card" id="org-vis-sees" aria-live="polite"></section>
      <section class="org-vis-card" id="org-vis-who-card" aria-live="polite"></section>
    </div>
    <p class="org-vis-note">${escapeHtml(vt('readonly_note'))}</p>`;
  host.replaceChildren(root);
  state = { host, root, view, isAdmin, userId: null, token: 0 };
  root.addEventListener('change', (e) => {
    if (e.target.id !== 'org-vis-person') return;
    state.userId = String(e.detail?.value ?? '') || null;
    load(state.userId);
  });
  if (isAdmin) fillPicker(view, isAdmin);
  await load(null);
}

/** Takes a newer structure into the picker; what is shown is read again from the server. */
export function refreshVisibilityTab({ view }) {
  if (!state) return;
  state.view = view;
  if (state.isAdmin) fillPicker(view, true);
  load(state.userId);
}

export function unmountVisibilityTab() {
  if (!state) return;
  state.host.replaceChildren();
  state = null;
}
