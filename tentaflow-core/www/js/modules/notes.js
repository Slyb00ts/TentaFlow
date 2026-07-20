// =============================================================================
// File: modules/notes.js — Per-user Notes app: sidebar list + markdown editor.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml, toast } from '/js/utils.js';
import { TfWindow } from '/js/components/tf-window.js';

const AUTOSAVE_DEBOUNCE_MS = 800;

let notes = [];
let activeNoteId = null;
let activeDetail = null;
let searchQuery = '';
let autosaveTimer = null;
let currentStatus = 'saved';

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function relativeLabel(epochSeconds) {
  if (!epochSeconds) return '';
  const diff = Math.floor(Date.now() / 1000) - Number(epochSeconds);
  if (diff < 60) return I18n.t('notes.relative_just_now');
  if (diff < 3600) {
    const n = Math.floor(diff / 60);
    return I18n.t('notes.relative_min_ago').replace('{n}', n);
  }
  if (diff < 86400) {
    const n = Math.floor(diff / 3600);
    return I18n.t('notes.relative_hours_ago').replace('{n}', n);
  }
  const n = Math.floor(diff / 86400);
  return I18n.t('notes.relative_days_ago').replace('{n}', n);
}

function wordCount(text) {
  if (!text) return 0;
  return text.trim().split(/\s+/).filter(Boolean).length;
}

const NotesScreen = {
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('mic')} ${escapeHtml(I18n.t('notes.page_title'))}</h1>
          <div class="sub">${escapeHtml(I18n.t('notes.subtitle'))} · <span id="notes-count">0</span></div>
        </div>
      </div>
      <div class="card" style="padding: 0; overflow: hidden;">
        <div class="notes-page">
          <aside class="notes-sidebar">
            <div class="notes-sidebar-header">
              <tf-searchbox id="notes-search" placeholder="${escapeHtml(I18n.t('notes.search_placeholder'))}" debounce="150"></tf-searchbox>
              <tf-button variant="primary" icon="plus" id="notes-new-btn">${escapeHtml(I18n.t('notes.new_button'))}</tf-button>
            </div>
            <div class="notes-list" id="notes-list"></div>
          </aside>
          <section class="notes-editor" id="notes-editor"></section>
        </div>
      </div>`;
  },

  async mount() {
    notes = [];
    activeNoteId = null;
    activeDetail = null;
    searchQuery = '';
    currentStatus = 'saved';

    byId('notes-new-btn')?.addEventListener('click', onCreateNote);
    const sb = byId('notes-search');
    sb?.addEventListener('search', (e) => {
      searchQuery = String(e.detail?.value ?? '').toLowerCase();
      renderList();
    });
    sb?.addEventListener('input', (e) => {
      searchQuery = String(e.detail?.value ?? '').toLowerCase();
      renderList();
    });

    await loadList();
    renderEmptyEditor();
  },

  unmount() {
    if (autosaveTimer) {
      clearTimeout(autosaveTimer);
      autosaveTimer = null;
    }
    notes = [];
    activeNoteId = null;
    activeDetail = null;
  },
};

async function loadList() {
  try {
    const resp = await ApiBinary.one('notesListRequest');
    notes = Array.isArray(resp?.notes) ? resp.notes : [];
    renderList();
  } catch (err) {
    toast(`${I18n.t('notes.status.error')}: ${err.message}`, 'error');
  }
}

function filteredNotes() {
  if (!searchQuery) return notes;
  const q = searchQuery;
  return notes.filter((n) => {
    const title = (n.title ?? '').toLowerCase();
    const preview = (n.bodyPreview ?? n.body_preview ?? '').toLowerCase();
    return title.includes(q) || preview.includes(q);
  });
}

function renderList() {
  const host = byId('notes-list');
  const countEl = byId('notes-count');
  if (countEl) countEl.textContent = I18n.t('notes.count').replace('{n}', notes.length);
  if (!host) return;
  const list = filteredNotes();
  if (list.length === 0) {
    host.innerHTML = `<div class="notes-empty">${escapeHtml(I18n.t('notes.empty_state'))}</div>`;
    return;
  }
  host.innerHTML = list.map(noteItemHtml).join('');
  host.querySelectorAll('[data-note-id]').forEach((el) => {
    el.addEventListener('click', (ev) => {
      if (ev.target.closest('[data-action]')) return;
      openNote(Number(el.dataset.noteId));
    });
  });
  host.querySelectorAll('[data-action="pin"]').forEach((b) => {
    b.addEventListener('click', async (ev) => {
      ev.stopPropagation();
      const id = Number(b.closest('[data-note-id]').dataset.noteId);
      const note = notes.find((n) => Number(n.id) === id);
      await togglePin(id, !note?.pinned);
    });
  });
  host.querySelectorAll('[data-action="delete"]').forEach((b) => {
    b.addEventListener('click', async (ev) => {
      ev.stopPropagation();
      const id = Number(b.closest('[data-note-id]').dataset.noteId);
      await deleteNoteFlow(id);
    });
  });
}

function noteItemHtml(n) {
  const id = Number(n.id);
  const active = id === activeNoteId ? ' active' : '';
  const title = (n.title && n.title.trim()) || I18n.t('notes.title_placeholder');
  const preview = n.bodyPreview ?? n.body_preview ?? '';
  const updated = Number(n.updatedAtEpoch ?? n.updated_at_epoch ?? 0);
  const pinIcon = n.pinned ? sprite('star') : '';
  return `
    <div class="note-item${active}" data-note-id="${id}">
      <div class="note-title">${pinIcon}${escapeHtml(title)}</div>
      ${preview ? `<div class="note-preview">${escapeHtml(preview)}</div>` : ''}
      <div class="note-time">${escapeHtml(relativeLabel(updated))}</div>
      <div class="note-actions">
        <tf-button variant="ghost" size="sm" icon="star" data-action="pin" aria-label="${escapeHtml(n.pinned ? I18n.t('notes.unpin') : I18n.t('notes.pin'))}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="trash" data-action="delete" aria-label="${escapeHtml(I18n.t('notes.delete'))}"></tf-button>
      </div>
    </div>`;
}

async function openNote(noteId) {
  await flushPendingAutosave();
  activeNoteId = noteId;
  try {
    const detail = await ApiBinary.one('noteDetailRequest', { noteId });
    activeDetail = {
      id: Number(detail.id),
      title: detail.title ?? '',
      body: detail.body ?? '',
      pinned: !!detail.pinned,
      updatedAtEpoch: Number(detail.updatedAtEpoch ?? detail.updated_at_epoch ?? 0),
    };
    renderEditor();
    renderList();
  } catch (err) {
    toast(`${I18n.t('notes.status.error')}: ${err.message}`, 'error');
  }
}

function renderEmptyEditor() {
  const host = byId('notes-editor');
  if (!host) return;
  host.innerHTML = `<div class="notes-editor-empty">${escapeHtml(I18n.t('notes.empty_editor'))}</div>`;
}

function renderEditor() {
  const host = byId('notes-editor');
  if (!host || !activeDetail) return;
  const d = activeDetail;
  const pinLabel = d.pinned ? I18n.t('notes.unpin') : I18n.t('notes.pin');
  host.innerHTML = `
    <tf-input id="note-title" size="lg" value="${escapeHtml(d.title)}" placeholder="${escapeHtml(I18n.t('notes.title_placeholder'))}"></tf-input>
    <div class="notes-editor-toolbar">
      <tf-button id="note-pin-btn" variant="secondary" icon="star">${escapeHtml(pinLabel)}</tf-button>
      <tf-button id="note-delete-btn" variant="danger" icon="trash">${escapeHtml(I18n.t('notes.delete'))}</tf-button>
      <span style="margin-left:auto;" id="note-word-count">${escapeHtml(I18n.t('notes.word_count').replace('{n}', wordCount(d.body)))}</span>
    </div>
    <tf-textarea id="note-body" rows="16" placeholder="${escapeHtml(I18n.t('notes.body_placeholder'))}">${escapeHtml(d.body)}</tf-textarea>
    <div class="notes-editor-footer">
      <span class="notes-status saved" id="note-status"><span class="notes-status-dot"></span><span id="note-status-label">${escapeHtml(I18n.t('notes.status.saved'))}</span></span>
      <span id="note-updated">${escapeHtml(relativeLabel(d.updatedAtEpoch))}</span>
    </div>`;

  const titleEl = byId('note-title');
  const bodyEl = byId('note-body');
  titleEl.addEventListener('input', onEditorInput);
  titleEl.addEventListener('change', flushPendingAutosave);
  bodyEl.addEventListener('input', onEditorInput);
  bodyEl.addEventListener('change', flushPendingAutosave);

  byId('note-pin-btn').addEventListener('click', async () => {
    await togglePin(activeDetail.id, !activeDetail.pinned);
  });
  byId('note-delete-btn').addEventListener('click', async () => {
    await deleteNoteFlow(activeDetail.id);
  });
}

function onEditorInput() {
  if (!activeDetail) return;
  const titleEl = byId('note-title');
  const bodyEl = byId('note-body');
  activeDetail.title = titleEl?.value ?? '';
  activeDetail.body = bodyEl?.value ?? '';
  const wc = byId('note-word-count');
  if (wc) wc.textContent = I18n.t('notes.word_count').replace('{n}', wordCount(activeDetail.body));
  setStatus('saving');
  if (autosaveTimer) clearTimeout(autosaveTimer);
  autosaveTimer = setTimeout(() => {
    autosaveTimer = null;
    persistActive().catch(() => {});
  }, AUTOSAVE_DEBOUNCE_MS);
}

async function flushPendingAutosave() {
  if (!autosaveTimer) return;
  clearTimeout(autosaveTimer);
  autosaveTimer = null;
  await persistActive();
}

async function persistActive() {
  if (!activeDetail) return;
  const noteId = activeDetail.id;
  try {
    const resp = await ApiBinary.action('noteUpdateRequest', {
      noteId,
      title: activeDetail.title,
      body: activeDetail.body,
    });
    const updated = Number(resp?.updatedAtEpoch ?? resp?.updated_at_epoch ?? 0);
    if (updated) activeDetail.updatedAtEpoch = updated;
    const idx = notes.findIndex((n) => Number(n.id) === noteId);
    if (idx >= 0) {
      notes[idx] = {
        ...notes[idx],
        title: activeDetail.title,
        bodyPreview: activeDetail.body.slice(0, 200),
        body_preview: activeDetail.body.slice(0, 200),
        updatedAtEpoch: activeDetail.updatedAtEpoch,
        updated_at_epoch: activeDetail.updatedAtEpoch,
      };
      sortNotes();
      renderList();
    }
    setStatus('saved');
    const up = byId('note-updated');
    if (up) up.textContent = relativeLabel(activeDetail.updatedAtEpoch);
  } catch (err) {
    setStatus('error');
    toast(`${I18n.t('notes.status.error')}: ${err.message}`, 'error');
  }
}

function sortNotes() {
  notes.sort((a, b) => {
    if (!!b.pinned - !!a.pinned !== 0) return (b.pinned ? 1 : 0) - (a.pinned ? 1 : 0);
    const bu = Number(b.updatedAtEpoch ?? b.updated_at_epoch ?? 0);
    const au = Number(a.updatedAtEpoch ?? a.updated_at_epoch ?? 0);
    return bu - au;
  });
}

function setStatus(kind) {
  currentStatus = kind;
  const el = byId('note-status');
  const lbl = byId('note-status-label');
  if (!el || !lbl) return;
  el.classList.remove('saving', 'saved', 'error');
  el.classList.add(kind);
  lbl.textContent = I18n.t(`notes.status.${kind}`);
}

async function onCreateNote() {
  await flushPendingAutosave();
  try {
    const resp = await ApiBinary.action('noteCreateRequest', { title: '', body: '' });
    const id = Number(resp?.id);
    if (!id) return;
    await loadList();
    await openNote(id);
    const titleEl = byId('note-title');
    if (titleEl && typeof titleEl.focus === 'function') titleEl.focus();
  } catch (err) {
    toast(`${I18n.t('notes.status.error')}: ${err.message}`, 'error');
  }
}

async function togglePin(noteId, pinned) {
  try {
    await ApiBinary.action('noteSetPinnedRequest', { noteId, pinned });
    const note = notes.find((n) => Number(n.id) === noteId);
    if (note) note.pinned = pinned;
    if (activeDetail && activeDetail.id === noteId) activeDetail.pinned = pinned;
    sortNotes();
    renderList();
    if (activeDetail && activeDetail.id === noteId) renderEditor();
  } catch (err) {
    toast(`${I18n.t('notes.status.error')}: ${err.message}`, 'error');
  }
}

async function deleteNoteFlow(noteId) {
  const ok = await TfWindow.confirm({
    title: I18n.t('notes.delete_confirm_title'),
    message: I18n.t('notes.delete_confirm_body'),
    confirmLabel: I18n.t('notes.delete'),
    cancelLabel: I18n.t('coming_soon.back') || 'Cancel',
    danger: true,
  });
  if (!ok) return;
  try {
    if (autosaveTimer) {
      clearTimeout(autosaveTimer);
      autosaveTimer = null;
    }
    await ApiBinary.action('noteDeleteRequest', { noteId });
    notes = notes.filter((n) => Number(n.id) !== noteId);
    if (activeDetail && activeDetail.id === noteId) {
      activeDetail = null;
      activeNoteId = null;
      renderEmptyEditor();
    }
    renderList();
  } catch (err) {
    toast(`${I18n.t('notes.status.error')}: ${err.message}`, 'error');
  }
}

export default NotesScreen;
