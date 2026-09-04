// ===== File: modules/tentaquant/dialogs.js — Q04 "Nowy projekt" and Q05 "Udostępnij projekt" =====
//
// Both windows edit ONE project and talk to the laboratory through the screen's
// `tq()` request helper, so the instance id and the mesh forwarding are decided
// in one place.
//
// Sharing follows ML Studio (§18 decision 15): the creator owns the project, it
// is private until shared, and a share is `editor` or `viewer` — never lab
// membership, which lives in the instance's permission matrix in Addons. A share
// to somebody the matrix does not admit is accepted and stored DORMANT, and the
// window says so instead of showing an access that does not exist.
//
// The person picker therefore searches EVERY TentaFlow account of the
// organization (`PeopleCandidates`), not the laboratory's roster: the people an
// owner wants to invite are simply the people who have an account, and whether
// the lab admits them yet is a warning on the row, not a reason to hide it.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import { T, sprite, fmtDate, errMessage, initials, has } from '/js/modules/tentaquant/format.js';

// =============================================================================
// Q04 — new project
// =============================================================================

export function openNewProjectWindow(screen) {
  const mayPublish = has(screen.lab?.myPermissions, 'quant.instruct');
  const state = { name: '', description: '', visibility: 'private', busy: false };

  const win = document.createElement('tf-window');
  win.className = 'tq-modal';
  win.setAttribute('title', T('new_project.title'));
  win.setAttribute('icon', 'plus');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '720');
  win.setAttribute('min-width', '320');

  const draw = () => {
    win.innerHTML = `
      <div slot="body">
        <tf-input id="tq-np-name" label="${escapeAttr(T('new_project.name_label'))}" placeholder="${escapeAttr(T('new_project.name_placeholder'))}" value="${escapeAttr(state.name)}" maxlength="120"></tf-input>
        <tf-textarea id="tq-np-desc" label="${escapeAttr(T('new_project.description_label'))}" placeholder="${escapeAttr(T('new_project.description_placeholder'))}" rows="2">${escapeHtml(state.description)}</tf-textarea>

        <div class="tq-field-label">${escapeHtml(T('new_project.start_label'))}</div>
        <tf-choice-group value="empty" columns="1">
          <tf-choice-card value="empty" icon="file" heading="${escapeAttr(T('new_project.start_empty_title'))}" description="${escapeAttr(T('new_project.start_empty_desc'))}"></tf-choice-card>
        </tf-choice-group>
        <div class="tq-field-hint">${escapeHtml(T('new_project.start_hint'))}</div>

        <div class="tq-field-label">${escapeHtml(T('new_project.visibility_label'))}</div>
        <tf-choice-group id="tq-np-visibility" value="${escapeAttr(state.visibility)}" columns="2">
          <tf-choice-card value="private" icon="lock" heading="${escapeAttr(T('new_project.visibility_private_title'))}" description="${escapeAttr(T('new_project.visibility_private_desc'))}"></tf-choice-card>
          <tf-choice-card value="lab" icon="eye" heading="${escapeAttr(T('new_project.visibility_lab_title'))}" description="${escapeAttr(T('new_project.visibility_lab_desc'))}"
            ${mayPublish ? '' : `disabled note="${escapeAttr(T('new_project.visibility_lab_locked'))}"`}></tf-choice-card>
        </tf-choice-group>
        <div class="tq-field-hint">${escapeHtml(T('new_project.hint'))}</div>
        <div class="tq-form-error" data-error hidden></div>
      </div>
      <div slot="footer">
        <tf-button variant="ghost" data-act="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
        <span class="tf-toolbar-spacer"></span>
        <tf-button variant="primary" icon="check" data-act="create" ${state.name.trim() && !state.busy ? '' : 'disabled'}>${escapeHtml(T('new_project.submit'))}</tf-button>
      </div>`;
    wire();
  };

  const syncSubmit = () => {
    const btn = win.querySelector('[data-act="create"]');
    if (state.name.trim() && !state.busy) btn.removeAttribute('disabled');
    else btn.setAttribute('disabled', '');
  };

  const wire = () => {
    const name = win.querySelector('#tq-np-name');
    const onName = () => { state.name = name.value; syncSubmit(); };
    name.addEventListener('input', onName);
    name.addEventListener('change', onName);
    const desc = win.querySelector('#tq-np-desc');
    desc.addEventListener('input', () => { state.description = desc.value; });
    desc.addEventListener('change', () => { state.description = desc.value; });
    win.querySelector('#tq-np-visibility').addEventListener('change', (e) => { state.visibility = e.detail.value; });
    win.querySelector('[data-act="cancel"]').addEventListener('click', () => win.close(true));
    win.querySelector('[data-act="create"]').addEventListener('click', create);
  };

  const create = async () => {
    if (state.busy || !state.name.trim()) return;
    state.busy = true;
    syncSubmit();
    const errBox = win.querySelector('[data-error]');
    errBox.hidden = true;
    try {
      const res = await screen.tq('tentaQuantProjectCreateRequest', {
        name: state.name.trim(),
        description: state.description.trim(),
        visibility: state.visibility,
      });
      toast(T('new_project.created', { name: res.project.name }), 'success');
      win.close(true);
      await screen.reloadProjects();
    } catch (e) {
      errBox.hidden = false;
      errBox.textContent = `${T('new_project.failed')}: ${errMessage(e)}`;
      state.busy = false;
      syncSubmit();
    }
  };

  draw();
  document.body.appendChild(win);
  return win;
}

// =============================================================================
// Q05 — share a project
// =============================================================================

// How many rows one search asks for. The server clamps to its own ceiling; this
// is what a picker can show without turning into a directory browser.
const CANDIDATE_LIMIT = 12;

function shareRowsHtml(project, shares, meId) {
  const owner = `
    <tr>
      <td>
        <div class="member-cell">
          <div class="member-avatar">${escapeHtml(initials(project.ownerName || project.ownerUserId))}</div>
          <div>
            <div class="member-name">${escapeHtml(project.ownerName || project.ownerUserId)}${project.ownerUserId === meId ? ` <span class="text-3">(${escapeHtml(T('share.you'))})</span>` : ''}</div>
            <div class="member-mail mono">${escapeHtml(project.ownerUserId)}</div>
          </div>
        </div>
      </td>
      <td><tf-chip status="accent" icon="crown" label="${escapeAttr(T('share.owner_chip'))}"></tf-chip></td>
      <td class="mono">—</td>
      <td class="mono">${escapeHtml(fmtDate(project.createdAt))}</td>
      <td class="tq-cell-right"><span class="text-3 text-xs">${escapeHtml(T('share.owner_note'))}</span></td>
    </tr>`;
  const rows = shares.map((s) => `
    <tr data-share-user="${escapeAttr(s.userId)}">
      <td>
        <div class="member-cell">
          <div class="member-avatar${s.hasLabAccess ? '' : ' muted'}">${escapeHtml(initials(s.displayName || s.userId))}</div>
          <div>
            <div class="member-name">${escapeHtml(s.displayName || s.userId)}</div>
            <div class="member-mail mono">${escapeHtml(s.userId)}</div>
          </div>
        </div>
      </td>
      <td>
        <tf-select data-role-for="${escapeAttr(s.userId)}" value="${escapeAttr(s.role)}">
          <option value="editor">${escapeHtml(T('share.role_editor'))}</option>
          <option value="viewer">${escapeHtml(T('share.role_viewer'))}</option>
        </tf-select>
      </td>
      <td>${escapeHtml(s.grantedBy || '—')}</td>
      <td class="mono">${escapeHtml(fmtDate(s.grantedAt))}</td>
      <td class="tq-cell-right">
        ${s.hasLabAccess ? '' : `<tf-chip status="warn" label="${escapeAttr(T('share.dormant_chip'))}" title="${escapeAttr(T('share.dormant_hint'))}"></tf-chip>`}
        <tf-button variant="ghost" size="sm" icon="trash" data-remove-share="${escapeAttr(s.userId)}">${escapeHtml(T('share.remove'))}</tf-button>
      </td>
    </tr>`).join('');
  return owner + rows;
}

export function openShareWindow(screen, projectId) {
  const state = {
    project: null, shares: [], people: [], peopleError: '', query: '', error: '',
  };
  const mayPublish = has(screen.lab?.myPermissions, 'quant.instruct');
  // Exactly ONE directory request in flight. A keystroke while a search runs
  // only replaces the pending query, so a slow answer for "ma" can never
  // overwrite the rows a later "marek" asked for.
  const search = { busy: false, pending: null };

  const win = document.createElement('tf-window');
  win.className = 'tq-modal tq-share';
  win.setAttribute('title', T('share.loading_title'));
  win.setAttribute('icon', 'share');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '860');
  win.setAttribute('min-width', '320');

  const dormant = () => state.shares.filter((s) => !s.hasLabAccess);

  const dormantHtml = () => (dormant().length
    ? `<tf-alert tone="warning" message="${escapeAttr(T('share.dormant_warning', { names: dormant().map((s) => s.displayName || s.userId).join(', '), n: dormant().length }))}"></tf-alert>`
    : '');

  const draw = () => {
    if (!state.project) {
      win.innerHTML = `<div slot="body"><div class="tq-loading">${escapeHtml(state.error || I18n.t('common.loading'))}</div></div>`;
      return;
    }
    win.setAttribute('title', T('share.title', { name: state.project.name }));
    // A plain <table class="tf-table"> rather than <tf-table>: every row hosts an
    // interactive tf-select bound to that person, which the tf-table row model
    // (values in, one row-click event out) does not carry. Same choice as the
    // access-keys and profiling-session tables.
    win.innerHTML = `
      <div slot="body">
        <div class="tq-field-label">${escapeHtml(T('share.people_label'))}</div>
        <div class="tq-table-scroll">
          <table class="tf-table tq-share-table">
            <thead><tr>
              <th>${escapeHtml(T('share.col_person'))}</th>
              <th>${escapeHtml(T('share.col_role'))}</th>
              <th>${escapeHtml(T('share.col_granted_by'))}</th>
              <th>${escapeHtml(T('share.col_granted_at'))}</th>
              <th></th>
            </tr></thead>
            <tbody id="tq-share-people">${shareRowsHtml(state.project, state.shares, screen.userId)}</tbody>
          </table>
        </div>

        <div class="tq-field-label">${escapeHtml(T('share.add_label'))}</div>
        <div class="invite-row">
          <tf-searchbox id="tq-share-search" placeholder="${escapeAttr(T('share.add_placeholder'))}" debounce="250" value="${escapeAttr(state.query)}"></tf-searchbox>
          <tf-select id="tq-share-role" value="viewer">
            <option value="viewer">${escapeHtml(T('share.role_viewer'))}</option>
            <option value="editor">${escapeHtml(T('share.role_editor'))}</option>
          </tf-select>
        </div>
        <div class="tq-candidates" id="tq-share-candidates" hidden></div>
        <div id="tq-share-dormant">${dormantHtml()}</div>

        <div class="toggle-row">
          <tf-toggle id="tq-share-lab" ${state.project.visibility === 'lab' ? 'checked' : ''} ${mayPublish ? '' : 'disabled'}></tf-toggle>
          <div>
            <div class="tr-name">${escapeHtml(T('share.lab_toggle_name'))}</div>
            <div class="tr-sub">${escapeHtml(mayPublish ? T('share.lab_toggle_sub', { n: screen.lab?.peopleCount ?? 0 }) : T('share.lab_toggle_locked'))}</div>
          </div>
        </div>

        <div class="tq-field-label">${escapeHtml(T('share.roles_label'))}</div>
        <div class="role-legend">
          <div class="rl"><div class="rt">${sprite('crown')}${escapeHtml(T('share.role_owner'))}</div><div class="rs">${escapeHtml(T('share.role_owner_desc'))}</div></div>
          <div class="rl"><div class="rt">${sprite('edit')}${escapeHtml(T('share.role_editor'))}</div><div class="rs">${escapeHtml(T('share.role_editor_desc'))}</div></div>
          <div class="rl"><div class="rt">${sprite('eye')}${escapeHtml(T('share.role_viewer'))}</div><div class="rs">${escapeHtml(T('share.role_viewer_desc'))}</div></div>
        </div>

        <tf-alert tone="info" message="${escapeAttr(T('share.callout'))}"></tf-alert>
        <div class="tq-form-error" data-error ${state.error ? '' : 'hidden'}>${escapeHtml(state.error)}</div>
      </div>
      <div slot="footer">
        <span class="tf-toolbar-spacer"></span>
        <tf-button variant="primary" data-act="close">${escapeHtml(T('share.close'))}</tf-button>
      </div>`;
    wire();
  };

  // Redraw of the people table alone. The whole window is NOT redrawn on a
  // mutation: that would throw away the search the user is in the middle of.
  const renderPeople = () => {
    const body = win.querySelector('#tq-share-people');
    if (!body) { draw(); return; }
    body.innerHTML = shareRowsHtml(state.project, state.shares, screen.userId);
    win.querySelector('#tq-share-dormant').innerHTML = dormantHtml();
    const errBox = win.querySelector('[data-error]');
    errBox.hidden = !state.error;
    errBox.textContent = state.error;
    wirePeople();
    if (state.query) renderCandidates();
  };

  const fail = (e, key) => {
    state.error = `${T(key)}: ${errMessage(e)}`;
    renderPeople();
  };

  const load = async () => {
    try {
      const res = await screen.tq('tentaQuantProjectGetRequest', { projectId });
      state.project = res.project;
      state.shares = res.shares || [];
      state.error = '';
    } catch (e) {
      state.error = `${T('share.load_failed')}: ${errMessage(e)}`;
    }
    draw();
  };

  const applyShares = (res) => {
    state.shares = res.shares || [];
    state.error = '';
    renderPeople();
  };

  const setShare = async (userId, role) => {
    try {
      applyShares(await screen.tq('tentaQuantProjectShareSetRequest', { projectId, userId, role }));
      await screen.reloadProjects();
    } catch (e) { fail(e, 'share.failed'); }
  };

  const removeShare = async (userId) => {
    try {
      applyShares(await screen.tq('tentaQuantProjectShareRemoveRequest', { projectId, userId }));
      await screen.reloadProjects();
    } catch (e) { fail(e, 'share.failed'); }
  };

  const setVisibility = async (visibility) => {
    try {
      const res = await screen.tq('tentaQuantProjectUpdateRequest', {
        projectId,
        name: state.project.name,
        description: state.project.description,
        visibility,
      });
      state.project = res.project;
      state.error = '';
      renderPeople();
      await screen.reloadProjects();
    } catch (e) {
      // The toggle already moved; put it back on the value the server still holds.
      const toggle = win.querySelector('#tq-share-lab');
      if (state.project.visibility === 'lab') toggle.setAttribute('checked', '');
      else toggle.removeAttribute('checked');
      fail(e, 'share.failed');
    }
  };

  // The directory search. It runs against the whole organization, so it is a
  // request per query rather than one list filtered locally — the answer is
  // bounded by the server and the searchbox debounces the typing.
  const runSearch = async () => {
    if (search.busy || search.pending === null) return;
    const query = search.pending;
    search.pending = null;
    search.busy = true;
    try {
      const res = await screen.tq('tentaQuantPeopleCandidatesRequest', { query, limit: CANDIDATE_LIMIT });
      state.people = res.people || [];
      state.peopleError = '';
    } catch (e) {
      state.people = [];
      state.peopleError = `${T('share.add_search_failed')}: ${errMessage(e)}`;
    }
    search.busy = false;
    // Only paint an answer that still matches what is in the box; a newer query
    // is already queued and repaints on its own.
    if (state.query === query) renderCandidates();
    runSearch();
  };

  const onSearch = (value) => {
    state.query = String(value || '').trim();
    if (!state.query) {
      search.pending = null;
      state.people = [];
      state.peopleError = '';
    } else {
      search.pending = state.query;
    }
    renderCandidates();
    runSearch();
  };

  // A person the laboratory does not admit is offered anyway — the share is
  // stored dormant and an administrator can grant the access later — so the row
  // carries the warning instead of disappearing.
  const candidateHtml = (u) => `
    <div class="tq-candidate${u.inLab ? '' : ' is-outside'}" data-candidate="${escapeAttr(u.userId)}" role="button" tabindex="0">
      <div class="member-avatar${u.inLab ? '' : ' muted'}">${escapeHtml(initials(u.displayName || u.userId))}</div>
      <div class="tq-candidate-meta">
        <div class="member-name">${escapeHtml(u.displayName || u.userId)}</div>
        <div class="member-mail mono">${escapeHtml(u.userId)}</div>
        ${u.inLab ? '' : `<div class="tq-candidate-warning">${sprite('alert')}${escapeHtml(T('share.candidate_no_lab', { name: u.displayName || u.userId }))}</div>`}
      </div>
      <tf-button variant="ghost" size="sm" icon="plus">${escapeHtml(T('share.add_button'))}</tf-button>
    </div>`;

  const renderCandidates = () => {
    const host = win.querySelector('#tq-share-candidates');
    if (!host) return;
    if (!state.query) {
      host.hidden = true;
      host.innerHTML = '';
      return;
    }
    host.hidden = false;
    if (state.peopleError) {
      host.innerHTML = `<div class="tq-field-hint">${escapeHtml(state.peopleError)}</div>`;
      return;
    }
    const taken = new Set([state.project.ownerUserId, ...state.shares.map((s) => s.userId)]);
    const rows = state.people.filter((u) => !taken.has(u.userId));
    if (!rows.length) {
      const pending = search.busy || search.pending !== null;
      host.innerHTML = `<div class="tq-field-hint">${escapeHtml(T(pending ? 'share.add_searching' : 'share.add_no_results'))}</div>`;
      return;
    }
    host.innerHTML = rows.map(candidateHtml).join('');
  };

  const wirePeople = () => {
    win.querySelectorAll('[data-role-for]').forEach((sel) => {
      sel.addEventListener('change', (e) => setShare(sel.dataset.roleFor, e.detail.value));
    });
    win.querySelectorAll('[data-remove-share]').forEach((btn) => {
      btn.addEventListener('click', () => removeShare(btn.dataset.removeShare));
    });
  };

  const wire = () => {
    // Every change is written by its own request the moment it is made, so the
    // window closes rather than saves — a "Zapisz" button would have nothing
    // left to do.
    win.querySelector('[data-act="close"]').addEventListener('click', () => win.close(true));
    wirePeople();
    win.querySelector('#tq-share-lab').addEventListener('change', (e) => {
      setVisibility(e.detail?.checked ? 'lab' : 'private');
    });
    win.querySelector('#tq-share-search').addEventListener('search', (e) => {
      onSearch(e.detail?.value);
    });

    // The rows carry `role="button" tabindex="0"`, so the keyboard has to reach
    // them the same way the pointer does.
    const pick = (row) => {
      if (row) setShare(row.dataset.candidate, win.querySelector('#tq-share-role').value || 'viewer');
    };
    const candidates = win.querySelector('#tq-share-candidates');
    candidates.addEventListener('click', (e) => pick(e.target.closest('[data-candidate]')));
    candidates.addEventListener('keydown', (e) => {
      if (e.key !== 'Enter' && e.key !== ' ') return;
      const row = e.target.closest('[data-candidate]');
      if (!row) return;
      e.preventDefault();
      pick(row);
    });
  };

  draw();
  document.body.appendChild(win);
  load();
  return win;
}

// =============================================================================
// Destructive confirmations
// =============================================================================

export function confirmDeleteProject(name) {
  return TfWindow.confirm({
    title: T('projects.delete_title'),
    // TfWindow.confirm inserts the message as HTML, so the name is escaped here.
    message: T('projects.delete_message', { name: escapeHtml(name) }),
    confirmLabel: T('projects.menu_delete'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
}
