// ===== File: modules/tentanas/share-users.js — the share users dialog: local SMB accounts (list, add, set password, delete) behind sudo =====
//
// MVP §7.1: share users are local accounts created by the admin. The password
// is sent once inside the request and never written to the state, logs or
// toasts — the dialog keeps it only in the two inputs until the request fires.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, ADMIN_TIMEOUT_MS, errMessage, fmtAgo } from '/js/modules/tentanas/format.js';
import { TfWindow } from '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';

export const PASSWORD_MIN = 8;
// Same rule Samba applies to local account names via useradd.
const USER_RE = /^[a-z_][a-z0-9_-]{0,31}$/;
export const shareUserNameValid = (name) => USER_RE.test(name);

/**
 * Validates the add / set-password form. Returns a map of field → error
 * message; an empty map means the form can be sent.
 */
export function validateUserForm({ name, password, repeat, existing = [], editing = false }) {
  const errors = {};
  if (!editing) {
    if (!shareUserNameValid(name)) errors.name = T('share_users.name_invalid');
    else if (existing.includes(name)) errors.name = T('share_users.name_taken');
  }
  const wantsPassword = !editing || password.length > 0 || repeat.length > 0;
  if (wantsPassword) {
    if (password.length < PASSWORD_MIN) errors.password = T('share_users.password_short', { n: PASSWORD_MIN });
    else if (password !== repeat) errors.repeat = T('share_users.password_mismatch');
  }
  return errors;
}

export function openShareUsersDialog(screen, { users = [], onChange = null } = {}) {
  const state = { users: users.slice(), form: null, busy: false };
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('share_users.title'));
  win.setAttribute('icon', 'users');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '680');
  win.setAttribute('min-width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  document.body.appendChild(win);

  const publish = () => { if (onChange) onChange(state.users.slice()); };

  const listHtml = () => `
    <div class="explain-box">${escapeHtml(T('share_users.explain'))}</div>
    <div class="section-card-head" style="padding:0">
      <div class="title">${escapeHtml(T('share_users.list_title'))}</div>
      <tf-chip size="sm" status="neutral" label="${state.users.length}"></tf-chip>
      <span class="spacer" style="flex:1"></span>
      <div class="actions"><tf-button size="sm" variant="primary" icon="plus" data-act="add" ${screen.isAdmin ? '' : 'disabled'}>${escapeHtml(T('share_users.add'))}</tf-button></div>
    </div>
    ${state.users.length ? `
    <tf-table id="nas-su-table" empty-message="${escapeAttr(T('share_users.empty'))}">
      <tf-column key="name" label="${escapeAttr(T('share_users.col_name'))}" renderer="html" fill></tf-column>
      <tf-column key="shares" label="${escapeAttr(T('share_users.col_shares'))}" renderer="html"></tf-column>
      <tf-column key="created" label="${escapeAttr(T('share_users.col_created'))}" renderer="text" nowrap hide-below="600"></tf-column>
    </tf-table>` : `
    <tf-empty-state icon="users" title="${escapeAttr(T('share_users.empty'))}" message="${escapeAttr(T('share_users.empty_msg'))}"></tf-empty-state>`}`;

  const formHtml = () => {
    const f = state.form;
    const editing = Boolean(f.editing);
    return `
      <div class="wizard-section-title">${escapeHtml(editing ? T('share_users.password_title', { name: f.name }) : T('share_users.add_title'))}</div>
      <div class="wizard-section-sub">${escapeHtml(editing ? T('share_users.password_sub') : T('share_users.add_sub'))}</div>
      <div class="stack">
        <tf-input id="nas-su-name" label="${escapeAttr(T('share_users.col_name'))}" placeholder="anna" autocomplete="off" spellcheck="false" autocapitalize="off" value="${escapeAttr(f.name)}" hint="${escapeAttr(T('share_users.name_hint'))}" ${editing ? 'readonly' : ''}></tf-input>
        <tf-input id="nas-su-desc" label="${escapeAttr(T('share_users.description'))}" placeholder="${escapeAttr(T('share_users.description_placeholder'))}" value="${escapeAttr(f.description)}"></tf-input>
        <div class="form-grid-2">
          <tf-input id="nas-su-pass" type="password" label="${escapeAttr(T('share_users.password'))}" autocomplete="new-password" hint="${escapeAttr(T('share_users.password_hint', { n: PASSWORD_MIN }))}"></tf-input>
          <tf-input id="nas-su-repeat" type="password" label="${escapeAttr(T('share_users.password_repeat'))}" autocomplete="new-password"></tf-input>
        </div>
        ${editing ? `<div class="muted">${escapeHtml(T('share_users.password_keep'))}</div>` : ''}
        <div class="num-err" id="nas-su-error" hidden></div>
      </div>`;
  };

  const footerHtml = () => (state.form
    ? `<tf-button variant="ghost" data-act="form-cancel" ${state.busy ? 'disabled' : ''}>${escapeHtml(I18n.t('common.cancel'))}</tf-button>
       <span class="spacer" style="flex:1"></span>
       <tf-button variant="primary" icon="check" data-act="form-save" disabled>${escapeHtml(state.form.editing ? T('share_users.save_password') : T('share_users.create'))}</tf-button>`
    : `<span class="spacer" style="flex:1"></span>
       <tf-button variant="secondary" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button>`);

  const draw = () => {
    win.innerHTML = `<div slot="body" class="stack">${state.form ? formHtml() : listHtml()}</div><div slot="footer">${footerHtml()}</div>`;
    if (state.form) wireForm(); else wireList();
  };

  const wireList = () => {
    win.querySelector('[data-act="add"]')?.addEventListener('click', () => { state.form = { editing: false, name: '', description: '' }; draw(); });
    const table = win.querySelector('#nas-su-table');
    if (!table) return;
    table.rows = state.users.map((u) => ({
      _user: u,
      name: `<div class="tf-table__cell-row">${sprite('user')}<span class="tf-table__cell-title tf-table__cell--mono">${escapeHtml(u.name)}</span></div>${u.description ? `<div class="tf-table__cell-sub">${escapeHtml(u.description)}</div>` : ''}`,
      shares: (u.shares || []).length ? u.shares.map((s) => `<tf-chip size="sm" status="neutral" label="${escapeAttr(s)}"></tf-chip>`).join(' ') : `<span class="muted">${escapeHtml(T('share_users.no_shares'))}</span>`,
      created: u.createdAt ? fmtAgo(u.createdAt) : '',
    }));
    table.rowActions = (row) => {
      const box = document.createElement('div');
      box.className = 'tf-table__cell-row';
      box.innerHTML = `
        <tf-button size="sm" variant="ghost" icon="key" data-act="password" title="${escapeAttr(T('share_users.set_password'))}" ${screen.isAdmin ? '' : 'disabled'}></tf-button>
        <tf-button size="sm" variant="ghost" icon="trash" tone="critical" data-act="delete" title="${escapeAttr(T('share_users.delete'))}" ${screen.isAdmin ? '' : 'disabled'}></tf-button>`;
      box.querySelector('[data-act="password"]').addEventListener('click', (e) => { e.stopPropagation(); state.form = { editing: true, name: row._user.name, description: row._user.description || '' }; draw(); });
      box.querySelector('[data-act="delete"]').addEventListener('click', (e) => { e.stopPropagation(); deleteUser(row._user); });
      return box;
    };
  };

  const wireForm = () => {
    const nameEl = win.querySelector('#nas-su-name');
    const descEl = win.querySelector('#nas-su-desc');
    const passEl = win.querySelector('#nas-su-pass');
    const repeatEl = win.querySelector('#nas-su-repeat');
    const save = win.querySelector('[data-act="form-save"]');
    const errEl = win.querySelector('#nas-su-error');
    const read = () => ({
      name: nameEl.value.trim(),
      description: descEl.value.trim(),
      password: passEl.value,
      repeat: repeatEl.value,
    });
    const sync = () => {
      const v = read();
      const errors = validateUserForm({ ...v, existing: state.users.map((u) => u.name), editing: state.form.editing });
      for (const [el, key] of [[nameEl, 'name'], [passEl, 'password'], [repeatEl, 'repeat']]) {
        // Only complain about a field the user has touched; a blank form
        // should not open covered in red.
        if (errors[key] && (key === 'name' ? v.name : v.password || v.repeat)) el.setAttribute('error', errors[key]);
        else el.removeAttribute('error');
      }
      if (Object.keys(errors).length || state.busy) save.setAttribute('disabled', '');
      else save.removeAttribute('disabled');
    };
    for (const el of [nameEl, descEl, passEl, repeatEl]) { el.addEventListener('input', sync); el.addEventListener('change', sync); }
    sync();
    win.querySelector('[data-act="form-cancel"]').addEventListener('click', () => { state.form = null; draw(); });
    save.addEventListener('click', async () => {
      const v = read();
      if (Object.keys(validateUserForm({ ...v, existing: state.users.map((u) => u.name), editing: state.form.editing })).length) return;
      state.busy = true;
      sync();
      errEl.hidden = true;
      const payload = { name: v.name, description: v.description };
      if (v.password) payload.password = v.password;
      try {
        const res = await screen.withSudo(
          (sudoPassword) => screen.nas('tentaNasShareUserSetRequest', { ...payload, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }),
          state.form.editing ? T('share_users.sudo_password', { name: v.name }) : T('share_users.sudo_add', { name: v.name }),
        );
        state.busy = false;
        if (!res) { sync(); return; }
        state.users = (res.users || []).slice().sort((a, b) => a.name.localeCompare(b.name));
        toast(state.form.editing ? T('share_users.password_saved', { name: v.name }) : T('share_users.created', { name: v.name }), 'success');
        state.form = null;
        publish();
        draw();
      } catch (e) {
        state.busy = false;
        errEl.textContent = errMessage(e);
        errEl.hidden = false;
        sync();
      }
    });
  };

  const deleteUser = async (user) => {
    const shares = user.shares || [];
    const ok = await TfWindow.confirm({
      title: T('share_users.delete_title', { name: user.name }),
      message: shares.length ? T('share_users.delete_msg_shares', { name: user.name, n: shares.length, shares: shares.join(', ') }) : T('share_users.delete_msg', { name: user.name }),
      confirmLabel: T('share_users.delete'),
      cancelLabel: I18n.t('common.cancel'),
      danger: true,
    });
    if (!ok) return;
    try {
      const res = await screen.withSudo(
        (sudoPassword) => screen.nas('tentaNasShareUserDeleteRequest', { name: user.name, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }),
        T('share_users.sudo_delete', { name: user.name }),
      );
      if (!res) return;
      state.users = (res.users || []).slice().sort((a, b) => a.name.localeCompare(b.name));
      toast(T('share_users.deleted', { name: user.name }), 'success');
      publish();
      if (win.isConnected) draw();
    } catch (e) {
      toast(errMessage(e), 'error');
    }
  };

  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'cancel') win.close(true);
  });
  draw();
  return win;
}
