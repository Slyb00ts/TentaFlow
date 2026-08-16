// ===== File: code-studio-panes.js — Code Studio stage panes and dock lists =====
//
// Content of the Code Studio shell: the file/editor pane, the patch-set review
// pane with per-hunk decisions (K02), the git pane driving a merge through an
// integration worktree (K03), the terminal pane, the commit pane, and the four
// dock lists that index them.
//
// The shell itself (tabs, drawer, composer, streams) belongs to code-studio.js;
// every entry point here takes a host element plus a context object and returns
// a handle:
//
//   ctx = { workspaceId, sessionId, workspace, session, openInStage, ask }
//   handle = { update(data), destroy() }
//
//   openInStage(stage, key, sub, extra) — 'plik' takes a path, 'zmiany' a path
//     plus { patchSetId, patchFileId }, 'terminal' a terminal id plus { label }.
//   ask({ capability, mandatoryInteractive, summary, detail, request }) — raises
//     the question in the composer; the shell sends `request` once the user says
//     yes, so a pane never performs a mandatory-interactive operation itself.
//
// `update(data)` merges a patch into the pane state and touches ONLY the DOM
// fragments the patch changed — a full re-render loses scroll position, the
// caret and expanded tree nodes.
//
// Every server round trip goes through the binary protocol (`ApiBinary` +
// the `codeStudio*` encoders in protocol/codec.js). Server text never reaches
// innerHTML: it is written through textContent (`el({ text })`) or through the
// `label` attribute of a tf-* component, both of which escape by construction.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { el, toast, formatRelative } from '/js/utils.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-tree.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-key-value.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-diff.js';
import '/js/components/tf-terminal.js';

const T = (key, vars) => I18n.t(key, vars);

// ---------------------------------------------------------------------------
// Protocol plumbing
// ---------------------------------------------------------------------------

function call(ctx, kind, payload = {}) {
  return ApiBinary.one(kind, {
    workspaceId: ctx.workspaceId,
    sessionId: ctx.sessionId,
    ...payload,
  });
}

function failed(err, fallbackKey) {
  toast(err && err.message ? err.message : T(fallbackKey), 'error');
}

// ---------------------------------------------------------------------------
// Small DOM helpers
// ---------------------------------------------------------------------------

const SVG_NS = 'http://www.w3.org/2000/svg';

function icon(name) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('class', 'icon');
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.appendChild(use);
  return svg;
}

function spacer() {
  return el('span', { class: 'spacer' });
}

function dockTitle(text) {
  return el('div', { class: 'cs-dock-title', text });
}

function hint(text) {
  return el('div', { class: 'cs-hint-keys', text });
}

// tf-button with slot content. The content must be in place before the element
// is connected — tf-button moves its light DOM inside the rendered <button> on
// first connect.
function button(opts, ...children) {
  const host = el('tf-button', {
    variant: opts.variant || 'secondary',
    size: opts.size || 'sm',
    class: opts.class || false,
    icon: opts.icon || false,
    disabled: opts.disabled ? '' : false,
    'data-nav': opts.nav ? '' : false,
  });
  for (const child of children) {
    if (child == null) continue;
    host.appendChild(typeof child === 'string' ? document.createTextNode(child) : child);
  }
  if (typeof opts.onClick === 'function') host.addEventListener('click', opts.onClick);
  return host;
}

// A dock list row. Rows are real controls (keyboard reachable), so they are
// tf-buttons wearing the row shape rather than clickable divs.
function rowButton(opts, ...children) {
  return button(
    {
      variant: 'ghost',
      size: 'sm',
      class: `cs-rowbtn${opts.active ? ' active' : ''}${opts.tone ? ` tone-${opts.tone}` : ''}`,
      onClick: opts.onClick,
    },
    ...children,
  );
}

function statusMark(kind) {
  return el('span', { class: 'pf' }, el('span', { class: `st ${kind.cls}`, text: kind.mark }));
}

function counts(add, del) {
  const wrap = el('span', { class: 'n' });
  if (add) wrap.appendChild(el('span', { class: 'plus', text: `+${add}` }));
  if (add && del) wrap.appendChild(document.createTextNode(' '));
  if (del) wrap.appendChild(el('span', { class: 'minus', text: `−${del}` }));
  return wrap;
}

// A pane head is narrow, so the path has to give way — but the FILE NAME is the
// part that identifies it. The directory shrinks and takes the ellipsis; the
// name never shrinks, so 390 px shows "…/README.md", never ".d".
function paintPath(host, path, emptyText) {
  const text = String(path || '');
  if (!text) {
    host.replaceChildren(el('span', { class: 'path-name', text: emptyText || '' }));
    host.setAttribute('title', emptyText || '');
    return;
  }
  const cut = text.lastIndexOf('/');
  const dir = cut === -1 ? '' : text.slice(0, cut + 1);
  const name = cut === -1 ? text : text.slice(cut + 1);
  const parts = [];
  if (dir) parts.push(el('span', { class: 'path-dir', text: dir }));
  parts.push(el('span', { class: 'path-name', text: name }));
  host.replaceChildren(...parts);
  host.setAttribute('title', text);
}

// Seven characters is what `git log --oneline` prints, what the merge steps
// quote and what the timeline shows — four was short enough to collide.
function shortSha(sha) {
  const text = String(sha || '');
  return text ? `${text.slice(0, 7)}…` : T('code_studio.panes.common.no_hash');
}

// Git hands out full 40-char oids; a list column reads them as noise. Seven
// characters is what `git log --oneline` shows and what the merge steps quote.
function shortOid(oid) {
  const text = String(oid || '');
  return text ? text.slice(0, 7) : T('code_studio.panes.common.no_hash');
}

// A worktree the session still has. `removed` and `detaching` rows are the
// journal of finished merge operations — the tree is gone from disk, so it is
// neither actionable nor "live". The git pane resolves the open merge with this
// same predicate, so a finished operation cannot be listed in the dock while the
// scene knows nothing about it.
function isLiveWorktree(wt) {
  const state = String(wt.state || '');
  return state !== 'removed' && state !== 'detaching';
}

// A worktree id is `<session uuid>` for the work tree and
// `<session uuid>-int-<op prefix>` for an integration one. Every row of the list
// belongs to the same session, so the row keeps only the part that differs.
function worktreeName(wt) {
  const id = String(wt.worktree_id || '');
  const cut = id.indexOf('-int-');
  return cut === -1 ? shortOid(id) : id.slice(cut + 1);
}

// A commit date arrives as an ISO string with the author's offset; the lists
// show it the way every other TentaFlow list does.
function relDate(iso) {
  const stamp = Date.parse(String(iso || ''));
  return Number.isNaN(stamp) ? T('code_studio.panes.common.no_hash') : formatRelative(stamp / 1000);
}

// tf-diff carries English fallbacks only, so every instance is handed the
// translated dict — the component is i18n-agnostic by design.
function diffLabels() {
  return {
    diff: T('code_studio.panes.diff.diff'),
    accept: T('code_studio.panes.diff.accept'),
    reject: T('code_studio.panes.diff.reject'),
    revert: T('code_studio.panes.diff.revert'),
    state_accepted: T('code_studio.panes.diff.state_accepted'),
    state_rejected: T('code_studio.panes.diff.state_rejected'),
    state_conflicted: T('code_studio.panes.diff.state_conflicted'),
    empty: T('code_studio.panes.diff.empty'),
  };
}

function diffView(attrs) {
  const view = el('tf-diff', attrs);
  view.labels = diffLabels();
  return view;
}

// ---------------------------------------------------------------------------
// File status vocabulary (decision #17: classes, never inline style)
// ---------------------------------------------------------------------------

const CHANGE_KIND = {
  add: { cls: 'a', mark: 'A', labelKey: 'code_studio.panes.change.add' },
  modify: { cls: 'm', mark: 'M', labelKey: 'code_studio.panes.change.modify' },
  rename: { cls: 'm', mark: 'M', labelKey: 'code_studio.panes.change.rename' },
  delete: { cls: 'd', mark: 'D', labelKey: 'code_studio.panes.change.delete' },
};
const CONFLICT_KIND = { cls: 'c', mark: '!', labelKey: 'code_studio.panes.change.conflict' };

function kindOf(file) {
  if (file.status === 'conflicted') return CONFLICT_KIND;
  return CHANGE_KIND[file.change_kind] || CHANGE_KIND.modify;
}

const PATCH_STATUSES = new Set([
  'open', 'in_review', 'accepted', 'partially_accepted', 'rejected', 'superseded', 'conflicted',
]);

function patchStatusLabel(status) {
  return PATCH_STATUSES.has(status)
    ? T(`code_studio.panes.patch_status.${status}`)
    : T('code_studio.panes.patch_status.unknown');
}

const TERMINAL_STATES = new Set(['idle', 'running', 'ok', 'error']);

function terminalStateLabel(state) {
  const key = TERMINAL_STATES.has(state) ? state : 'idle';
  return T(`code_studio.panes.terminal.state.${key}`);
}

// ---------------------------------------------------------------------------
// Unified-diff parsing. A hunk body carries BOTH sides, so the same text feeds
// the reviewable diff and the reconstruction preview without a second request.
// ---------------------------------------------------------------------------

const HUNK_HEADER_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

function hunkLines(hunk) {
  const match = HUNK_HEADER_RE.exec(String(hunk.header || ''));
  let oldLn = match ? Number(match[1]) : 1;
  let newLn = match ? Number(match[2]) : 1;
  const rows = String(hunk.content ?? '').split('\n');
  if (rows.length && rows[rows.length - 1] === '') rows.pop();
  const out = [];
  for (const row of rows) {
    // The wire repeats the `@@` header as the first row of the body. It is
    // positioning metadata, not a line of the file: emitting it would print the
    // header twice and shift every number in the hunk by one.
    const inner = HUNK_HEADER_RE.exec(row);
    if (inner) {
      oldLn = Number(inner[1]);
      newLn = Number(inner[2]);
      continue;
    }
    const mark = row.charAt(0);
    const text = row.slice(1);
    if (mark === '\\') continue; // "\ No newline at end of file"
    // tf-diff numbers a row from `oldLn`/`newLn`, so both sides travel with the
    // line — an added row has no line on the old side, and vice versa.
    if (mark === '+') out.push({ kind: 'add', oldLn: null, newLn: newLn++, text });
    else if (mark === '-') out.push({ kind: 'del', oldLn: oldLn++, newLn: null, text });
    else out.push({ kind: 'ctx', oldLn: oldLn++, newLn: newLn++, text });
  }
  return out;
}

function hunkStats(hunk) {
  let add = 0;
  let del = 0;
  for (const line of hunkLines(hunk)) {
    if (line.kind === 'add') add += 1;
    else if (line.kind === 'del') del += 1;
  }
  return { add, del };
}

function fileStats(file) {
  let add = 0;
  let del = 0;
  for (const hunk of file.hunks || []) {
    const s = hunkStats(hunk);
    add += s.add;
    del += s.del;
  }
  return { add, del };
}

function setStats(files) {
  let add = 0;
  let del = 0;
  for (const file of files) {
    const s = fileStats(file);
    add += s.add;
    del += s.del;
  }
  return { add, del };
}

// ---------------------------------------------------------------------------
// Per-session review state. The stage pane and the dock list are separate
// instances of separate functions; a decision taken in one has to move the
// counters in the other without either re-rendering the whole panel.
// ---------------------------------------------------------------------------

const reviewBuses = new Map();

function busFor(ctx) {
  const key = `${ctx.workspaceId}/${ctx.sessionId}`;
  let bus = reviewBuses.get(key);
  if (!bus) {
    bus = {
      key,
      listeners: new Set(),
      patchSet: null,
      files: [],
      selectedFileId: null,
      decisions: new Map(), // patch_hunk_id -> 'accept' | 'reject'
    };
    reviewBuses.set(key, bus);
  }
  return bus;
}

function busSubscribe(bus, fn) {
  bus.listeners.add(fn);
  return () => {
    bus.listeners.delete(fn);
    if (bus.listeners.size === 0) reviewBuses.delete(bus.key);
  };
}

function busEmit(bus, topic) {
  for (const fn of [...bus.listeners]) fn(topic, bus);
}

function decisionOf(bus, hunk) {
  const local = bus.decisions.get(hunk.patch_hunk_id);
  if (local) return local;
  if (hunk.status === 'accepted') return 'accept';
  if (hunk.status === 'rejected') return 'reject';
  return null;
}

// ONE counting function for every hunk counter on the screen. The head chip
// counts the hunks of the open file, the footer and the legend count the whole
// patch set — the same arithmetic, only a different list of hunks, so the three
// readings can never disagree.
function tallyHunks(bus, hunks) {
  let accepted = 0;
  let rejected = 0;
  let pending = 0;
  for (const hunk of hunks) {
    const decision = decisionOf(bus, hunk);
    if (decision === 'accept') accepted += 1;
    else if (decision === 'reject') rejected += 1;
    else pending += 1;
  }
  return {
    accepted,
    rejected,
    pending,
    decided: accepted + rejected,
    total: accepted + rejected + pending,
  };
}

function setHunks(bus) {
  const all = [];
  for (const file of bus.files) all.push(...(file.hunks || []));
  return all;
}

function tallyDecisions(bus) {
  return tallyHunks(bus, setHunks(bus));
}

// Builds the PatchDecideRequest body from the decisions taken so far. Files
// without a single decided hunk are left out — an untouched file must not be
// silently swept into a verdict.
function decisionPayload(bus) {
  const files = [];
  for (const file of bus.files) {
    const hunks = [];
    let accepted = 0;
    for (const hunk of file.hunks || []) {
      const decision = decisionOf(bus, hunk);
      if (!decision) continue;
      hunks.push({ patchHunkId: hunk.patch_hunk_id, decision });
      if (decision === 'accept') accepted += 1;
    }
    if (!hunks.length) continue;
    files.push({
      patchFileId: file.patch_file_id,
      decision: accepted > 0 ? 'accept' : 'reject',
      hunks,
    });
  }
  return files;
}

async function loadPatchSet(ctx, bus, patchSetId) {
  const body = await call(ctx, 'codeStudioPatchSetGetRequest', { patchSetId });
  bus.patchSet = body.patch_set || null;
  bus.files = Array.isArray(body.files) ? body.files : [];
  bus.decisions.clear();
  busEmit(bus, 'loaded');
  return bus;
}

async function saveDecisions(ctx, bus) {
  const files = decisionPayload(bus);
  if (!files.length) {
    toast(T('code_studio.panes.changes.nothing_decided'), 'info');
    return;
  }
  const body = await call(ctx, 'codeStudioPatchDecideRequest', {
    patchSetId: bus.patchSet.patch_set_id,
    files,
  });
  const conflicted = Array.isArray(body.conflicted_paths) ? body.conflicted_paths : [];
  if (conflicted.length) {
    toast(T('code_studio.panes.changes.saved_with_conflicts', { count: conflicted.length }), 'warning');
  } else if (['accepted', 'partially_accepted'].includes(String(body.status ?? ''))) {
    // The commit is a separate decision; saying so here is the difference
    // between "nothing happened" and "one step left".
    toast(T('code_studio.panes.changes.saved_needs_commit'), 'success');
  } else {
    toast(T('code_studio.panes.changes.saved'), 'success');
  }
  await loadPatchSet(ctx, bus, bus.patchSet.patch_set_id);
  return body.status ?? '';
}

/// Commits an already-accepted set through the same call the agent would make.
async function commitAccepted(ctx, bus) {
  const message = String(bus.commitMessage ?? '').trim()
    || T('code_studio.panes.changes.commit_default_message');
  await call(ctx, 'codeStudioGitCommitRequest', { message });
  toast(T('code_studio.panes.changes.committed'), 'success');
}

// ===========================================================================
// File pane — editor over the session worktree
// ===========================================================================

export function renderFilePane(hostEl, ctx) {
  const { head, body } = paneShell(hostEl, { withFoot: false });
  let disposed = false;

  const pathLabel = el('span', { class: 'path' });
  paintPath(pathLabel, '', T('code_studio.panes.file.none'));
  const statusChip = el('tf-chip', { status: 'neutral', label: T('code_studio.panes.file.clean') });
  const saveBtn = button(
    { variant: 'secondary', onClick: () => save() },
    T('code_studio.panes.file.save'),
  );
  // Right-hand side: the file drawer slides in from the right (decision #1).
  const treeBtn = button(
    { variant: 'secondary', icon: 'folder', nav: true },
    T('code_studio.panes.file.tree'),
  );
  head.append(pathLabel, statusChip, spacer(), treeBtn, saveBtn);

  const tabsEl = el('tf-tabs', { variant: 'underline', class: 'cs-file-tabs' });
  const editor = el('tf-code-editor', { 'aria-label': T('code_studio.panes.file.editor_label') });
  const conflictSlot = el('div', { class: 'cs-conflict-slot' });
  body.append(tabsEl, conflictSlot, editor);

  // path -> { path, blobSha, content, language, dirty, tabId }
  const open = new Map();
  let activePath = null;
  let tabSeq = 0;

  editor.addEventListener('change', () => {
    const entry = activePath ? open.get(activePath) : null;
    if (!entry) return;
    const dirty = editor.value !== entry.content;
    if (dirty === entry.dirty) return;
    entry.dirty = dirty;
    paintTab(entry);
    paintStatus();
  });
  editor.addEventListener('save', () => save());
  tabsEl.addEventListener('change', (e) => {
    const id = e.detail && e.detail.value;
    for (const entry of open.values()) {
      if (entry.tabId === id) {
        activate(entry.path);
        return;
      }
    }
  });

  function paintTab(entry) {
    const tab = tabsEl.querySelector(`#${CSS.escape(entry.tabId)}`);
    if (!tab) return;
    const name = entry.path.split('/').pop();
    // Leading bullet marks unsaved content; tf-tabs has no dirty flag yet.
    tab.setAttribute('label', entry.dirty ? `• ${name}` : name);
  }

  function paintStatus() {
    const entry = activePath ? open.get(activePath) : null;
    if (!entry) {
      statusChip.setAttribute('status', 'neutral');
      statusChip.setAttribute('label', T('code_studio.panes.file.clean'));
      return;
    }
    statusChip.setAttribute('status', entry.dirty ? 'warn' : 'ok');
    statusChip.setAttribute(
      'label',
      entry.dirty
        ? T('code_studio.panes.file.unsaved')
        : T('code_studio.panes.file.saved_sha', { sha: shortSha(entry.blobSha) }),
    );
  }

  function activate(path) {
    const entry = open.get(path);
    if (!entry) return;
    // Park the outgoing buffer so switching tabs never drops unsaved text.
    if (activePath && activePath !== path && open.has(activePath)) {
      open.get(activePath).draft = editor.value;
    }
    activePath = path;
    editor.setAttribute('language', entry.language || 'plain');
    editor.value = entry.draft != null ? entry.draft : entry.content;
    editor.markClean();
    entry.dirty = editor.value !== entry.content;
    tabsEl.setAttribute('value', entry.tabId);
    paintPath(pathLabel, entry.path);
    conflictSlot.replaceChildren();
    paintTab(entry);
    paintStatus();
  }

  async function openPath(path) {
    if (open.has(path)) {
      activate(path);
      return;
    }
    try {
      const body_ = await call(ctx, 'codeStudioFileReadRequest', { path });
      if (disposed) return;
      const entry = {
        path: body_.path || path,
        content: body_.content || '',
        draft: null,
        blobSha: body_.blob_sha || '',
        language: body_.language || 'plain',
        truncated: !!body_.truncated,
        dirty: false,
        tabId: `cs-file-tab-${(tabSeq += 1)}`,
      };
      open.set(entry.path, entry);
      const tab = el('tf-tab', { id: entry.tabId, label: entry.path.split('/').pop() });
      tabsEl.appendChild(tab);
      activate(entry.path);
      if (entry.truncated) toast(T('code_studio.panes.file.truncated'), 'info');
    } catch (err) {
      failed(err, 'code_studio.panes.file.read_failed');
    }
  }

  // A rejected write is never retried without the sha: we re-read the file and
  // compare hashes, so the user sees an actual CAS conflict instead of a
  // generic error, and nothing is overwritten behind their back.
  async function save() {
    const entry = activePath ? open.get(activePath) : null;
    if (!entry) return;
    const content = editor.value;
    try {
      const body_ = await call(ctx, 'codeStudioFileWriteRequest', {
        path: entry.path,
        content,
        expectedBlobSha: entry.blobSha,
      });
      if (disposed) return;
      entry.content = content;
      entry.draft = null;
      entry.blobSha = body_.blob_sha || entry.blobSha;
      entry.dirty = false;
      editor.markClean();
      conflictSlot.replaceChildren();
      paintTab(entry);
      paintStatus();
      toast(T('code_studio.panes.file.saved'), 'success');
    } catch (err) {
      await reportWriteFailure(entry, err);
    }
  }

  async function reportWriteFailure(entry, err) {
    let onDisk = null;
    try {
      onDisk = await call(ctx, 'codeStudioFileReadRequest', { path: entry.path });
    } catch (readErr) {
      failed(readErr, 'code_studio.panes.file.read_failed');
    }
    if (disposed) return;
    if (!onDisk || onDisk.blob_sha === entry.blobSha) {
      failed(err, 'code_studio.panes.file.write_failed');
      return;
    }
    showCasConflict(entry, onDisk);
  }

  function showCasConflict(entry, onDisk) {
    const box = el('div', { class: 'confl' });
    box.append(
      el('h4', {}, icon('alert'), document.createTextNode(T('code_studio.panes.file.cas_title'))),
      el('p', {
        text: T('code_studio.panes.file.cas_body', {
          expected: shortSha(entry.blobSha),
          actual: shortSha(onDisk.blob_sha),
        }),
      }),
      el(
        'div',
        { class: 'cs-actions' },
        button(
          {
            variant: 'secondary',
            onClick: () => {
              conflictSlot.replaceChildren();
            },
          },
          T('code_studio.panes.file.cas_keep_editing'),
        ),
        button(
          {
            variant: 'primary',
            onClick: () => {
              entry.content = onDisk.content || '';
              entry.blobSha = onDisk.blob_sha || '';
              entry.draft = null;
              entry.dirty = false;
              editor.value = entry.content;
              editor.markClean();
              conflictSlot.replaceChildren();
              paintTab(entry);
              paintStatus();
            },
          },
          T('code_studio.panes.file.cas_reload'),
        ),
      ),
    );
    conflictSlot.replaceChildren(box);
  }

  return {
    update(data = {}) {
      if (disposed) return;
      if (data.path) openPath(data.path);
      if (data.closePath && open.has(data.closePath)) {
        const entry = open.get(data.closePath);
        const tab = tabsEl.querySelector(`#${CSS.escape(entry.tabId)}`);
        if (tab) tab.remove();
        open.delete(data.closePath);
        if (activePath === data.closePath) {
          activePath = null;
          const next = open.keys().next();
          if (!next.done) activate(next.value);
          else {
            editor.value = '';
            paintPath(pathLabel, '', T('code_studio.panes.file.none'));
            paintStatus();
          }
        }
      }
    },
    destroy() {
      disposed = true;
      open.clear();
      hostEl.replaceChildren();
    },
  };
}

// ===========================================================================
// Changes pane (K02) — per-hunk review, CAS conflict, reconstruction
// ===========================================================================

export function renderChangesPane(hostEl, ctx) {
  const { head, body, foot } = paneShell(hostEl, { withFoot: true });
  const bus = busFor(ctx);
  let disposed = false;
  let activeFileId = null;

  const pathLabel = el('span', { class: 'path' });
  paintPath(pathLabel, '', T('code_studio.panes.changes.none'));
  const kindChip = el('tf-chip', { status: 'neutral', label: T('code_studio.panes.changes.none_short') });
  const progressChip = el('tf-chip', { status: 'info', label: '' });
  const listBtn = button(
    { variant: 'secondary', icon: 'list', nav: true },
    T('code_studio.panes.changes.list'),
  );
  head.append(pathLabel, kindChip, spacer(), progressChip, listBtn);

  const conflictSlot = el('div', { class: 'cs-conflict-slot' });
  const diffEl = diffView({ mode: 'unified', reviewable: '' });
  const reconSlot = el('div', { class: 'cs-recon-slot' });
  body.append(conflictSlot, diffEl, reconSlot);

  const footHint = el('span', { class: 'cs-hint-keys', text: '' });
  const reviseBtn = button({ variant: 'secondary', onClick: () => toggleRevisionBox() },
    T('code_studio.panes.changes.request_revision'));
  const saveBtn = button({ variant: 'primary', icon: 'check', onClick: () => onSave() },
    T('code_studio.panes.changes.save_decisions'));
  // Accepting a review does NOT commit: `commit_accepted_blobs` is reached from
  // the git_commit path, so without this the operator accepts everything and
  // nothing visibly happens (§11.5). The button carries that second decision.
  const commitBtn = button(
    { variant: 'primary', icon: 'git', onClick: () => onCommit() },
    T('code_studio.panes.changes.commit'),
  );
  commitBtn.setAttribute('hidden', '');

  foot.append(footHint, spacer(), reviseBtn, saveBtn, commitBtn);

  const revisionBox = el('div', { class: 'cs-revise-box', hidden: '' });
  const revisionText = el('tf-textarea', {
    rows: '2',
    placeholder: T('code_studio.panes.changes.revision_placeholder'),
  });
  revisionBox.append(
    revisionText,
    el(
      'div',
      { class: 'cs-actions' },
      button({ variant: 'secondary', onClick: () => toggleRevisionBox(false) },
        T('code_studio.panes.common.cancel')),
      button({ variant: 'primary', onClick: () => sendRevision() },
        T('code_studio.panes.changes.revision_send')),
    ),
  );
  body.appendChild(revisionBox);

  diffEl.addEventListener('hunk-decide', (e) => {
    const detail = e.detail || {};
    if (!detail.hunkId) return;
    if (detail.decision === 'accept' || detail.decision === 'reject') {
      bus.decisions.set(detail.hunkId, detail.decision);
    } else {
      bus.decisions.delete(detail.hunkId);
    }
    // Only the counters and the reconstruction move — the diff owns its own
    // hunk chrome, and re-assigning `hunks` here would repaint the whole list
    // and lose scroll.
    paintProgress();
    paintRecon();
    busEmit(bus, 'decisions');
  });

  const unsubscribe = busSubscribe(bus, (topic) => {
    if (disposed) return;
    if (topic === 'select') {
      activeFileId = bus.selectedFileId;
      paintFile();
    } else if (topic === 'loaded') {
      paintFile();
    } else {
      paintProgress();
    }
  });

  function activeFile() {
    if (!bus.files.length) return null;
    return bus.files.find((f) => f.patch_file_id === activeFileId) || bus.files[0];
  }

  function paintProgress() {
    const file = activeFile();
    const set = tallyDecisions(bus);
    // An accepted set still needs a commit; show the action only then, so the
    // button never invites a commit of something nobody reviewed.
    const decidedStatus = String(bus.patchSet?.status ?? '');
    if (['accepted', 'partially_accepted'].includes(decidedStatus)) {
      commitBtn.removeAttribute('hidden');
    } else {
      commitBtn.setAttribute('hidden', '');
    }
    footHint.textContent = T('code_studio.panes.changes.decided_of', {
      decided: set.decided,
      total: set.total,
      count: set.total,
    });
    if (!file) {
      progressChip.setAttribute('label', T('code_studio.panes.changes.none_short'));
      return;
    }
    const own = tallyHunks(bus, file.hunks || []);
    progressChip.setAttribute(
      'label',
      T('code_studio.panes.changes.hunks_in_file', {
        decided: own.decided,
        total: own.total,
        count: own.total,
      }),
    );
  }

  function paintFile() {
    const file = activeFile();
    conflictSlot.replaceChildren();
    reconSlot.replaceChildren();
    if (!file) {
      diffEl.hunks = [];
      paintPath(pathLabel, '', T('code_studio.panes.changes.none'));
      kindChip.setAttribute('status', 'neutral');
      kindChip.setAttribute('label', T('code_studio.panes.changes.none_short'));
      paintProgress();
      return;
    }
    activeFileId = file.patch_file_id;
    paintPath(pathLabel, file.path);
    const kind = kindOf(file);
    const stats = fileStats(file);
    kindChip.setAttribute('status', file.status === 'conflicted' ? 'err' : 'ok');
    kindChip.setAttribute(
      'label',
      `${T(kind.labelKey)} · +${stats.add} −${stats.del}`,
    );

    diffEl.hunks = (file.hunks || []).map((hunk) => ({
      id: hunk.patch_hunk_id,
      header: hunk.header,
      lines: hunkLines(hunk),
      status: decisionOf(bus, hunk) === 'accept'
        ? 'accepted'
        : decisionOf(bus, hunk) === 'reject' ? 'rejected' : 'pending',
    }));

    if (file.status === 'conflicted') renderConflict(file);
    paintRecon();
    paintProgress();
  }

  // CAS conflict on a patch file: the set was built on one content hash, the
  // worktree now holds another. §13.2 forbids guessing — the decision moves up
  // to whole-file level.
  function renderConflict(file) {
    const box = el('div', { class: 'confl' });
    box.append(
      el('h4', {}, icon('alert'), document.createTextNode(T('code_studio.panes.changes.conflict_title'))),
      el('p', {
        text: T('code_studio.panes.changes.conflict_body', {
          base: shortSha(file.patch_base_blob_sha),
          current: shortSha(file.current_blob_sha),
        }),
      }),
      el(
        'div',
        { class: 'cs-actions' },
        button({ variant: 'secondary', onClick: () => showDiskDiff(file) },
          T('code_studio.panes.changes.conflict_show_disk')),
        button({ variant: 'danger', onClick: () => rejectFile(file) },
          T('code_studio.panes.changes.conflict_reject')),
        button({ variant: 'primary', onClick: () => toggleRevisionBox(true) },
          T('code_studio.panes.changes.conflict_reprepare')),
      ),
    );
    conflictSlot.appendChild(box);
  }

  async function showDiskDiff(file) {
    try {
      const body_ = await call(ctx, 'codeStudioGitDiffRequest', { path: file.path });
      if (disposed) return;
      const wrap = el('div', { class: 'diff' });
      wrap.appendChild(
        el('div', { class: 'diff-head' }, icon('code'), document.createTextNode(
          T('code_studio.panes.changes.disk_diff_head', { path: file.path }),
        )),
      );
      const view = diffView({ mode: 'unified' });
      view.hunks = (body_.hunks || []).map((h, idx) => ({
        id: `disk-${idx}`,
        header: h.header,
        lines: hunkLines(h),
        status: 'pending',
      }));
      wrap.appendChild(view);
      if (body_.truncated) wrap.appendChild(hint(T('code_studio.panes.changes.disk_diff_truncated')));
      conflictSlot.appendChild(wrap);
    } catch (err) {
      failed(err, 'code_studio.panes.changes.disk_diff_failed');
    }
  }

  async function rejectFile(file) {
    if (!bus.patchSet) return;
    try {
      await call(ctx, 'codeStudioPatchDecideRequest', {
        patchSetId: bus.patchSet.patch_set_id,
        files: [{ patchFileId: file.patch_file_id, decision: 'reject', hunks: [] }],
      });
      await loadPatchSet(ctx, bus, bus.patchSet.patch_set_id);
    } catch (err) {
      failed(err, 'code_studio.panes.changes.decide_failed');
    }
  }

  // The reconstruction follows every decision, so it is rebuilt on its own —
  // it is the only fragment a decision changes besides the counters.
  function paintRecon() {
    reconSlot.replaceChildren();
    const file = activeFile();
    if (file) renderReconstruction(file);
  }

  // Reconstruction after a partial acceptance: accepted hunks are composed onto
  // the BASE content, not onto the file as it is on disk, so a rejected hunk
  // cannot slip in through the side door. Both hashes are named explicitly.
  function renderReconstruction(file) {
    const hunks = file.hunks || [];
    const accepted = hunks.filter((h) => decisionOf(bus, h) === 'accept');
    const rejected = hunks.filter((h) => decisionOf(bus, h) === 'reject');
    if (!accepted.length || !rejected.length) return;

    // Nobody asked for this view — it appears because the decisions split the
    // file — so it has to open by saying what happened, what the two columns
    // are and why they differ, before the reader starts comparing code.
    reconSlot.append(
      dockTitle(T('code_studio.panes.changes.recon_title')),
      el('div', {
        class: 'cs-note',
        text: T('code_studio.panes.changes.recon_body', {
          base: shortSha(file.patch_base_blob_sha),
          accepted: accepted.length,
          rejected: rejected.length,
        }),
      }),
    );

    const baseLines = [];
    const resultLines = [];
    for (const hunk of accepted) {
      for (const line of hunkLines(hunk)) {
        // The base column is a listing of the base file: it keeps the base
        // numbering and renders the old gutter alone.
        if (line.kind !== 'add') {
          baseLines.push({ kind: 'ctx', oldLn: line.oldLn, newLn: null, text: line.text });
        }
        if (line.kind !== 'del') resultLines.push(line);
      }
    }

    const grid = el('div', { class: 'recon' });
    grid.append(
      reconColumn(
        'file-text',
        T('code_studio.panes.changes.recon_base', { sha: shortSha(file.patch_base_blob_sha) }),
        baseLines,
        'old',
      ),
      reconColumn(
        'check',
        T('code_studio.panes.changes.recon_result', {
          sha: shortSha(file.accepted_blob_sha || file.current_blob_sha),
        }),
        resultLines,
        'new',
      ),
    );
    reconSlot.appendChild(grid);
    reconSlot.appendChild(
      hint(T('code_studio.panes.changes.recon_excluded', {
        count: rejected.length,
      })),
    );
  }

  // Each column lists ONE version of the file, so it carries the one line-number
  // column of that version — a second, mirrored gutter would only claim width.
  function reconColumn(iconName, title, lines, gutters) {
    const wrap = el('div', { class: 'diff' });
    wrap.appendChild(
      el('div', { class: 'diff-head' }, icon(iconName), document.createTextNode(title)),
    );
    const view = diffView({ mode: 'unified', gutters });
    view.hunks = [{ id: `recon-${iconName}`, header: '', lines, status: 'pending' }];
    wrap.appendChild(view);
    return wrap;
  }

  function toggleRevisionBox(force) {
    const show = force === undefined ? revisionBox.hasAttribute('hidden') : !!force;
    if (show) revisionBox.removeAttribute('hidden');
    else revisionBox.setAttribute('hidden', '');
  }

  async function sendRevision() {
    const file = activeFile();
    if (!file || !bus.patchSet) return;
    const note = String(revisionText.value || '').trim();
    if (!note) {
      toast(T('code_studio.panes.changes.revision_empty'), 'warning');
      return;
    }
    try {
      await call(ctx, 'codeStudioPatchDecideRequest', {
        patchSetId: bus.patchSet.patch_set_id,
        files: [{ patchFileId: file.patch_file_id, decision: 'request_revision', note, hunks: [] }],
      });
      revisionText.value = '';
      toggleRevisionBox(false);
      await loadPatchSet(ctx, bus, bus.patchSet.patch_set_id);
    } catch (err) {
      failed(err, 'code_studio.panes.changes.decide_failed');
    }
  }

  async function onCommit() {
    try {
      await commitAccepted(ctx, bus);
      await loadPatchSet(ctx, bus, bus.patchSet.patch_set_id);
      paintProgress();
    } catch (e) {
      // An approval gate answers with Conflict, which is not a failure: the
      // operator has to decide, and the session view renders that card.
      toast(String(e?.message ?? e), 'warning');
    }
  }

  async function onSave() {
    if (!bus.patchSet) return;
    try {
      await saveDecisions(ctx, bus);
    } catch (err) {
      failed(err, 'code_studio.panes.changes.decide_failed');
    }
  }

  paintFile();

  return {
    update(data = {}) {
      if (disposed) return;
      if (data.patchFileId) {
        activeFileId = data.patchFileId;
        paintFile();
      }
      if (data.patchSetId && (!bus.patchSet || bus.patchSet.patch_set_id !== data.patchSetId)) {
        loadPatchSet(ctx, bus, data.patchSetId).catch((err) =>
          failed(err, 'code_studio.panes.changes.load_failed'));
      }
    },
    destroy() {
      disposed = true;
      unsubscribe();
      hostEl.replaceChildren();
    },
  };
}

// ===========================================================================
// Git pane (K03) — merge through a detached integration worktree
// ===========================================================================

const MERGE_STEPS = [
  'integration_worktree',
  'merge',
  'tests',
  'review',
  'approval',
  'update_ref',
];

export function renderGitPane(hostEl, ctx) {
  const { head, body, foot } = paneShell(hostEl, { withFoot: true });
  let disposed = false;
  let merge = null; // GitMergeResponse + { finalizeStatus, patchSet }

  const titleWrap = el('span', {});
  const titleMain = el('strong', { class: 'cs-pane-title', text: T('code_studio.panes.git.merge_title') });
  const titleSub = el('span', { class: 'cs-stage-sub', text: '' });
  titleWrap.append(titleMain, titleSub);
  const stateChip = el('tf-chip', { status: 'warn', label: T('code_studio.panes.git.needs_confirm') });
  // The branch, its worktrees and its history are a LIST; the merge steps carry
  // the decision. On a phone the scene keeps the screen and the list slides in
  // from the right as a drawer, the same way the file tree and the change list
  // already do — otherwise a merge could be inspected but never finished there.
  const dockBtn = button(
    { variant: 'secondary', icon: 'branch', nav: true },
    T('code_studio.panes.git.dock_nav'),
  );
  head.append(titleWrap, spacer(), dockBtn, stateChip);

  const noticeSlot = el('div', { class: 'cs-notice-slot' });
  const stepsSlot = el('div', { class: 'cs-steps-slot' });
  const conflictSlot = el('div', { class: 'cs-conflict-slot' });
  body.append(noticeSlot, stepsSlot, conflictSlot);

  const footHint = el('span', { class: 'cs-hint-keys', text: T('code_studio.panes.git.foot_hint') });
  const abandonBtn = button({ variant: 'secondary', onClick: () => abandon() },
    T('code_studio.panes.git.abandon'));
  const primaryBtn = button({ variant: 'primary', icon: 'check', class: 'cs-btn-ask', onClick: () => askMerge() },
    T('code_studio.panes.git.merge_action', { target: T('code_studio.panes.git.target_fallback') }));
  foot.append(footHint, spacer(), abandonBtn, primaryBtn);

  function targetBranch() {
    return (merge && merge.target_branch)
      || (ctx.workspace && ctx.workspace.target_branch)
      || T('code_studio.panes.git.target_fallback');
  }

  // ONE answer per step: the badge and the sentence under it are produced
  // together, so a step can never wear a state its own line contradicts. Every
  // value comes from what the wire actually reports — `unknown` exists because
  // the merge answer carries no test verdict, and "not reported" is a fact while
  // "running" would be a guess. Before any merge exists nothing has started, so
  // no step claims to be in progress.
  function stepStatus(name) {
    const nothing = T('code_studio.panes.git.fact_not_started');
    if (!merge) return { state: 'wait', fact: nothing };
    const patchSet = merge.patchSet;
    const patchStatus = patchSet ? patchSet.status : null;
    const handedOver = patchStatus === 'in_review'
      || patchStatus === 'accepted'
      || patchStatus === 'partially_accepted';
    const accepted = patchStatus === 'accepted' || patchStatus === 'partially_accepted';
    const clean = merge.outcome === 'clean';
    switch (name) {
      case 'integration_worktree': {
        const wt = merge.integration_worktree;
        if (!wt) return { state: 'wait', fact: nothing };
        return {
          state: 'done',
          fact: T('code_studio.panes.git.step_worktree_fact', {
            worktree: worktreeName(wt),
            base: shortOid(wt.base_commit || merge.expected_old),
          }),
        };
      }
      case 'merge': {
        if (merge.outcome === 'conflict') {
          const files = Array.isArray(merge.conflict_files) ? merge.conflict_files : [];
          return {
            state: 'failed',
            fact: files.length
              ? T('code_studio.panes.git.step_merge_fact_conflict', {
                count: files.length, files: files.join(', '),
              })
              : T('code_studio.panes.git.step_merge_fact_conflict_unknown'),
          };
        }
        return {
          state: 'done',
          fact: T('code_studio.panes.git.step_merge_fact_clean', {
            ref: `refs/code-studio/integration/${shortOid(merge.op_id)}`,
          }),
        };
      }
      case 'tests':
        return clean
          ? { state: 'unknown', fact: T('code_studio.panes.git.step_tests_fact_none') }
          : { state: 'wait', fact: nothing };
      case 'review':
        if (!clean || !patchSet) return { state: 'wait', fact: nothing };
        return {
          state: handedOver ? 'done' : 'now',
          fact: T('code_studio.panes.git.step_review_fact', {
            set: shortOid(patchSet.patch_set_id),
            status: patchStatusLabel(patchStatus),
          }),
        };
      case 'approval':
        if (accepted) {
          return {
            state: 'done',
            fact: T('code_studio.panes.git.step_approval_fact_accepted', {
              who: (patchSet && patchSet.decided_by) || T('code_studio.panes.git.who_unknown'),
            }),
          };
        }
        if (handedOver) {
          return { state: 'now', fact: T('code_studio.panes.git.step_approval_fact_waiting') };
        }
        return { state: 'wait', fact: nothing };
      case 'update_ref':
        if (merge.finalizeStatus === 'merged') {
          return {
            state: 'done',
            fact: T('code_studio.panes.git.step_update_ref_fact_done', { target: targetBranch() }),
          };
        }
        if (merge.finalizeStatus === 'stale_base') {
          return {
            state: 'failed',
            fact: T('code_studio.panes.git.step_update_ref_fact_stale', { target: targetBranch() }),
          };
        }
        return {
          state: accepted ? 'now' : 'wait',
          fact: T('code_studio.panes.git.step_update_ref_fact_guard', {
            target: targetBranch(),
            base: merge.expected_old
              ? shortOid(merge.expected_old)
              : T('code_studio.panes.git.base_pending'),
          }),
        };
      default:
        return { state: 'wait', fact: nothing };
    }
  }

  function paintSteps() {
    const frag = document.createDocumentFragment();
    frag.appendChild(dockTitle(T('code_studio.panes.git.steps_title')));
    MERGE_STEPS.forEach((name, idx) => {
      const { state, fact } = stepStatus(name);
      const step = el('div', { class: `mstep ${state}` });
      const col = el('span', { class: 'mstep-col' }, el('span', { class: 'num', text: String(idx + 1) }));
      if (idx < MERGE_STEPS.length - 1) col.appendChild(el('span', { class: 'rail' }));
      step.append(
        col,
        el(
          'span',
          { class: 'mstep-txt' },
          el(
            'span',
            { class: 'mstep-line' },
            el('h4', { text: T(`code_studio.panes.git.step_${name}`) }),
            // The state is spelled out: a border and a dimmed row are not a
            // reading of "done", "running" or "still waiting".
            el('span', { class: `mstep-state ${state}`, text: T(`code_studio.panes.git.state_${state}`) }),
          ),
          el('p', { text: fact }),
        ),
      );
      frag.appendChild(step);
    });
    stepsSlot.replaceChildren(frag);
  }

  function paintNotice() {
    noticeSlot.replaceChildren();
    if (!merge) return;
    if (merge.finalizeStatus === 'stale_base') {
      const box = el('div', { class: 'confl' });
      box.append(
        el('h4', {}, icon('alert'), document.createTextNode(T('code_studio.panes.git.stale_title'))),
        el('p', { text: T('code_studio.panes.git.stale_body', { target: targetBranch() }) }),
      );
      noticeSlot.appendChild(box);
    }
  }

  // A conflict is a RESULT, not a failed operation: the integration worktree
  // stays `held` so the next revision run has something to work on, and the
  // target branch is untouched.
  function paintConflict() {
    conflictSlot.replaceChildren();
    if (!merge || merge.outcome !== 'conflict') return;
    const box = el('div', { class: 'confl recon-warn' });
    box.append(
      el('h4', {}, icon('alert'), document.createTextNode(T('code_studio.panes.git.conflict_title'))),
      el('p', { text: T('code_studio.panes.git.conflict_body') }),
    );
    conflictSlot.appendChild(box);
    const files = Array.isArray(merge.conflict_files) ? merge.conflict_files : [];
    conflictSlot.appendChild(dockTitle(T('code_studio.panes.git.conflict_files', {
      count: files.length,
    })));
    // A worklist, one row per path. An empty list is a state of the merge, not
    // an aside about where the paths come from.
    if (!files.length) {
      conflictSlot.appendChild(
        el('div', { class: 'cs-note', text: T('code_studio.panes.git.conflict_files_none') }),
      );
    }
    // Opening the file is the ONLY way out of the conflict, so the row wears an
    // action, not a dim caption: an 10.5px grey word is not an affordance for
    // the single next step of a stopped merge.
    for (const path of files) {
      conflictSlot.appendChild(
        rowButton(
          { tone: 'err', onClick: () => ctx.openInStage('plik', path, T('code_studio.panes.git.conflict_file_sub')) },
          statusMark(CONFLICT_KIND),
          el('span', { class: 'cs-row-label', text: path }),
          el('span', { class: 'cs-row-go' },
            document.createTextNode(T('code_studio.panes.git.conflict_file_open')),
            icon('chevron-right')),
        ),
      );
    }
    conflictSlot.appendChild(dockTitle(T('code_studio.panes.git.conflict_next_title')));
    conflictSlot.appendChild(el('div', { class: 'cs-note', text: T('code_studio.panes.git.conflict_next_body') }));
  }

  function paintFooter() {
    const conflict = merge && merge.outcome === 'conflict';
    footHint.textContent = conflict
      ? T('code_studio.panes.git.foot_hint_conflict')
      : T('code_studio.panes.git.foot_hint');
    primaryBtn.setAttribute('label', conflict
      ? T('code_studio.panes.git.delegate_conflict')
      : T('code_studio.panes.git.merge_action', { target: targetBranch() }));
    stateChip.setAttribute('status', conflict ? 'err' : 'warn');
    stateChip.setAttribute('label', conflict
      ? T('code_studio.panes.git.conflict_chip')
      : T('code_studio.panes.git.needs_confirm'));
    titleMain.textContent = T('code_studio.panes.git.merge_title_to', { target: targetBranch() });
    titleSub.textContent = merge && merge.expected_old
      ? T('code_studio.panes.git.merge_sub', { base: shortOid(merge.expected_old) })
      : T('code_studio.panes.git.merge_sub_pending');
    abandonBtn.toggleAttribute('disabled', !merge || !merge.op_id);
  }

  function paint() {
    paintNotice();
    paintSteps();
    paintConflict();
    paintFooter();
  }

  /// The branch a merge would read FROM. Before the first merge exists there is
  /// no `merge` object to read it off, and the session branch is the answer —
  /// `loadMerge` records exactly that value afterwards. Sending an empty string
  /// instead made the broker refuse an invalid ref, surfacing as an opaque
  /// "internal" toast, so a merge could never be started from the dashboard.
  function sourceBranch() {
    return (merge && merge.source_branch) || (ctx.session && ctx.session.branch) || '';
  }

  // The button never merges. `git_merge` is mandatory_interactive (§9.3 step 5)
  // and the question cannot be switched off, so this routes to the composer.
  function askMerge() {
    if (merge && merge.outcome === 'conflict') {
      delegateConflict();
      return;
    }
    const source = sourceBranch();
    if (!merge?.op_id && !source) {
      toast(T('code_studio.panes.git.no_source_branch'), 'error');
      return;
    }
    ctx.ask({
      capability: 'git_merge',
      mandatoryInteractive: true,
      summary: T('code_studio.panes.git.ask_merge_summary', { target: targetBranch() }),
      detail: T('code_studio.panes.git.ask_merge_detail', { target: targetBranch() }),
      request: {
        kind: merge && merge.op_id ? 'codeStudioGitMergeFinalizeRequest' : 'codeStudioGitMergeRequest',
        payload: merge && merge.op_id
          ? { opId: merge.op_id, patchSetId: merge.patch_set_id }
          : { sourceBranch: source, targetBranch: targetBranch() },
      },
    });
  }

  async function delegateConflict() {
    const files = merge && Array.isArray(merge.conflict_files) ? merge.conflict_files : [];
    const worktree = merge && merge.integration_worktree
      ? merge.integration_worktree.worktree_id
      : shortOid(merge && merge.op_id);
    try {
      await call(ctx, 'codeStudioSessionMessageSendRequest', {
        message: files.length
          ? T('code_studio.panes.git.delegate_message', {
            target: targetBranch(),
            files: files.join(', '),
          })
          : T('code_studio.panes.git.delegate_message_unknown', {
            target: targetBranch(),
            worktree,
          }),
      });
      toast(T('code_studio.panes.git.delegate_sent'), 'success');
    } catch (err) {
      failed(err, 'code_studio.panes.git.delegate_failed');
    }
  }

  // Routed through the composer for the same reason `askMerge` is: calling the
  // API straight from here swallowed an `approval_required` answer into a toast,
  // so the first click appeared to do nothing and only a second one — after the
  // question had been answered elsewhere — took effect.
  function abandon() {
    if (!merge || !merge.op_id) return;
    ctx.ask({
      capability: 'git_merge',
      summary: T('code_studio.panes.git.ask_abandon_summary', { target: targetBranch() }),
      detail: T('code_studio.panes.git.ask_abandon_detail'),
      request: {
        kind: 'codeStudioGitMergeAbandonRequest',
        payload: { opId: merge.op_id },
      },
    });
  }

  // A merge answer is delivered once, to the browser that asked for it. Everyone
  // else — a reload, a second operator, the phone — reads the same merge off the
  // session state: an integration worktree that is still alive IS the open
  // merge, its `held` state IS the conflict the broker stopped on, and its
  // `conflict_files` ARE the paths that conflict.
  async function loadMerge() {
    try {
      const [worktrees, sets] = await Promise.all([
        call(ctx, 'codeStudioWorktreesListRequest', {}),
        call(ctx, 'codeStudioPatchSetsListRequest', { status: '' }),
      ]);
      if (disposed) return;
      const list = Array.isArray(worktrees.worktrees) ? worktrees.worktrees : [];
      const integration = list.find((wt) => wt.purpose === 'integration' && isLiveWorktree(wt));
      if (!integration) {
        merge = null;
        paint();
        return;
      }
      const mergeSets = (Array.isArray(sets.patch_sets) ? sets.patch_sets : [])
        .filter((set) => set.scope === 'merge')
        .sort((a, b) => String(b.created_at).localeCompare(String(a.created_at)));
      const patchSet = mergeSets[0] || null;
      // A finalize told us the target had moved; refreshing the same operation
      // must not throw that away, because no read request reports it.
      const same = merge && merge.op_id === integration.op_id;
      const livePaths = same && Array.isArray(merge.conflict_files) ? merge.conflict_files : [];
      // The worktree row carries the paths the broker recorded, so they survive
      // a reload; the live merge answer is only preferred while it is the
      // fresher one for the SAME operation.
      const persistedPaths = Array.isArray(integration.conflict_files) ? integration.conflict_files : [];
      const knownFinalize = same ? merge.finalizeStatus || '' : '';
      merge = {
        op_id: integration.op_id || '',
        outcome: integration.state === 'held' ? 'conflict' : 'clean',
        conflict_files: livePaths.length ? livePaths : persistedPaths,
        patch_set_id: patchSet ? patchSet.patch_set_id : null,
        integration_worktree: integration,
        // `head_commit` of an integration worktree is the target tip the merge
        // was computed on — the value `update-ref` will be checked against.
        expected_old: integration.head_commit || '',
        source_branch: (ctx.session && ctx.session.branch) || '',
        patchSet,
        finalizeStatus: knownFinalize,
      };
      paint();
    } catch (err) {
      failed(err, 'code_studio.panes.git.load_failed');
    }
  }

  paint();
  void loadMerge();

  return {
    update(data = {}) {
      if (disposed) return;
      if ('merge' in data) merge = data.merge;
      if (merge && data.finalizeStatus) merge.finalizeStatus = data.finalizeStatus;
      if (merge && data.patchSet) merge.patchSet = data.patchSet;
      if (data.refresh) { void loadMerge(); return; }
      paint();
    },
    destroy() {
      disposed = true;
      hostEl.replaceChildren();
    },
  };
}

// ===========================================================================
// Terminal pane — server-side VT, the component only renders and sends keys
// ===========================================================================

export function renderTerminalPane(hostEl, ctx) {
  const { head, body, foot } = paneShell(hostEl, { withFoot: true });
  let disposed = false;
  let terminalId = null;
  let revision = 0;
  let grid = { rows: 24, cols: 80 };

  const profileChip = el('tf-chip', { status: 'accent', label: T('code_studio.panes.terminal.profile_pending') });
  const titleEl = el('span', { class: 'cs-stage-sub', text: T('code_studio.panes.terminal.shell') });
  const sessionsBtn = button({ variant: 'secondary', icon: 'list', nav: true },
    T('code_studio.panes.terminal.sessions'));
  const interruptBtn = button({ variant: 'danger', icon: 'stop', onClick: () => interrupt() },
    T('code_studio.panes.terminal.interrupt'));
  head.append(profileChip, titleEl, spacer(), sessionsBtn, interruptBtn);

  const termEl = el('tf-terminal', { rows: String(grid.rows), cols: String(grid.cols) });
  body.appendChild(termEl);

  // §19 / §26 pt. 5: the sandbox has no git metadata, so `git` works through the
  // broker shim while `.git/` simply is not there. Saying it once, in place.
  foot.appendChild(hint(T('code_studio.panes.terminal.no_git_metadata')));

  termEl.addEventListener('key', (e) => {
    const bytes = e.detail && e.detail.bytes;
    if (!terminalId || bytes == null) return;
    call(ctx, 'codeStudioTerminalInputRequest', { terminalId, data: bytes })
      .catch((err) => failed(err, 'code_studio.panes.terminal.input_failed'));
  });

  function measureGrid() {
    const style = getComputedStyle(termEl);
    const fontSize = parseFloat(style.fontSize) || 12;
    const lineHeight = parseFloat(style.lineHeight) || fontSize * 1.35;
    const canvas = measureGrid.canvas || (measureGrid.canvas = document.createElement('canvas'));
    const context = canvas.getContext('2d');
    context.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
    const cell = context.measureText('M').width || fontSize * 0.6;
    const rect = termEl.getBoundingClientRect();
    // A pane that has not been laid out yet reports zero — opening an 0x0 grid
    // would make the server wrap every line of the first command.
    if (rect.height < lineHeight || rect.width < cell) return { rows: 24, cols: 80 };
    return {
      rows: Math.max(4, Math.floor(rect.height / lineHeight)),
      cols: Math.max(20, Math.floor(rect.width / cell)),
    };
  }

  const observer = new ResizeObserver(() => {
    if (disposed || !terminalId) return;
    const next = measureGrid();
    if (next.rows === grid.rows && next.cols === grid.cols) return;
    grid = next;
    termEl.setAttribute('rows', String(grid.rows));
    termEl.setAttribute('cols', String(grid.cols));
    call(ctx, 'codeStudioTerminalResizeRequest', { terminalId, rows: grid.rows, cols: grid.cols })
      .catch((err) => failed(err, 'code_studio.panes.terminal.resize_failed'));
  });
  observer.observe(termEl);

  function paintProfile(mount, network) {
    profileChip.setAttribute(
      'label',
      T('code_studio.panes.terminal.profile', {
        mount: mount || 'cow',
        network: network || 'none',
      }),
    );
  }

  async function openTerminal() {
    try {
      grid = measureGrid();
      const body_ = await call(ctx, 'codeStudioTerminalOpenRequest', {
        rows: grid.rows,
        cols: grid.cols,
      });
      if (disposed) return;
      terminalId = body_.terminal_id;
      grid = { rows: body_.rows || grid.rows, cols: body_.cols || grid.cols };
      termEl.setAttribute('rows', String(grid.rows));
      termEl.setAttribute('cols', String(grid.cols));
      paintProfile(body_.mount_access, body_.network_access);
      await pullSnapshot();
    } catch (err) {
      failed(err, 'code_studio.panes.terminal.open_failed');
    }
  }

  // Reconnect pulls the WHOLE grid plus its revision; the live stream then
  // carries only changed rows.
  async function pullSnapshot() {
    if (!terminalId) return;
    try {
      const snap = await call(ctx, 'codeStudioTerminalSnapshotRequest', { terminalId });
      if (disposed) return;
      revision = Number(snap.revision || 0);
      termEl.applySnapshot({
        revision,
        cursor: {
          row: Number(snap.cursor_row || 0),
          col: Number(snap.cursor_col || 0),
          visible: !!snap.cursor_visible,
        },
        rows: Array.isArray(snap.cells) ? snap.cells : [],
      });
    } catch (err) {
      failed(err, 'code_studio.panes.terminal.snapshot_failed');
    }
  }

  // Interrupt is a key press, not a separate capability: the VT machine on the
  // owner node turns ETX into SIGINT for the foreground process.
  function interrupt() {
    if (!terminalId) return;
    call(ctx, 'codeStudioTerminalInputRequest', { terminalId, data: '\u0003' })
      .catch((err) => failed(err, 'code_studio.panes.terminal.input_failed'));
  }

  return {
    update(data = {}) {
      if (disposed) return;
      if (data.terminalId && data.terminalId !== terminalId) {
        terminalId = data.terminalId;
        revision = 0;
        pullSnapshot();
      }
      if (!terminalId && data.open) openTerminal();
      if (data.title) titleEl.textContent = data.title;
      if (data.mountAccess || data.networkAccess) paintProfile(data.mountAccess, data.networkAccess);
      if (data.delta) {
        const next = Number(data.delta.revision || 0);
        // An out-of-order delta means the grid moved past us: re-pull instead of
        // painting a hole.
        if (next && next <= revision) return;
        revision = next || revision;
        termEl.applyChanges(data.delta);
      }
      if (data.resync) pullSnapshot();
    },
    destroy() {
      disposed = true;
      observer.disconnect();
      hostEl.replaceChildren();
    },
  };
}

// ===========================================================================
// Commit pane — what the commit is, and where it came from
// ===========================================================================

export function renderCommitPane(hostEl, ctx) {
  const { head, body } = paneShell(hostEl, { withFoot: false });
  let disposed = false;
  let commit = null;
  let patchSet = null;
  let files = [];

  const subject = el('strong', { class: 'cs-pane-title', text: T('code_studio.panes.commit.none') });
  const meta = el('span', { class: 'cs-stage-sub', text: '' });
  const statChip = el('tf-chip', { status: 'neutral', label: '' });
  const copyBtn = button({ variant: 'ghost', icon: 'copy', onClick: () => copyOid() });
  head.append(el('span', {}, subject, meta), spacer(), statChip, copyBtn);

  const originTitle = dockTitle(T('code_studio.panes.commit.origin_title'));
  const originKv = el('tf-key-value', {});
  const filesTitle = dockTitle(T('code_studio.panes.commit.files_title'));
  const filesSlot = el('div', { class: 'cs-commit-files' });
  body.append(originTitle, originKv, filesTitle, filesSlot);

  function copyOid() {
    if (!commit || !navigator.clipboard) return;
    navigator.clipboard.writeText(commit.oid || '')
      .then(() => toast(T('code_studio.panes.commit.copied'), 'success'))
      .catch(() => toast(T('code_studio.panes.commit.copy_failed'), 'error'));
  }

  function paint() {
    subject.textContent = commit ? commit.subject : T('code_studio.panes.commit.none');
    meta.textContent = commit
      ? T('code_studio.panes.commit.meta', { sha: shortOid(commit.oid), date: relDate(commit.date) })
      : '';

    const stats = setStats(files);
    statChip.setAttribute(
      'label',
      T('code_studio.panes.commit.stats', {
        files: files.length,
        count: files.length,
        add: stats.add,
        del: stats.del,
      }),
    );

    const branch = (ctx.session && ctx.session.branch) || T('code_studio.panes.commit.branch_unknown');
    const base = patchSet ? shortOid(patchSet.base_commit) : T('code_studio.panes.commit.base_unknown');
    originKv.entries = [
      { key: T('code_studio.panes.commit.author'), value: commit ? commit.author : '—' },
      { key: T('code_studio.panes.commit.branch'), value: branch },
      { key: T('code_studio.panes.commit.base'), value: base },
      {
        key: T('code_studio.panes.commit.source'),
        value: patchSet
          ? T('code_studio.panes.commit.source_value', { id: patchSet.patch_set_id })
          : T('code_studio.panes.commit.source_unknown'),
      },
      {
        key: T('code_studio.panes.commit.review'),
        value: patchSet
          ? patchStatusLabel(patchSet.status)
          : T('code_studio.panes.commit.review_unknown'),
        chip: patchSet ? T('code_studio.panes.commit.review_by', {
          who: patchSet.decided_by || T('code_studio.panes.commit.review_nobody'),
        }) : null,
        chipTone: 'ok',
      },
    ];

    const frag = document.createDocumentFragment();
    for (const file of files) {
      const kind = kindOf(file);
      const stat = fileStats(file);
      frag.appendChild(
        rowButton(
          {
            onClick: () => ctx.openInStage('zmiany', file.path, T(kind.labelKey), {
              patchSetId: patchSet ? patchSet.patch_set_id : '',
              patchFileId: file.patch_file_id,
            }),
          },
          statusMark(kind),
          el('span', { class: 'cs-row-label', text: file.path }),
          counts(stat.add, stat.del),
        ),
      );
    }
    if (!files.length) {
      frag.appendChild(el('tf-empty-state', {
        icon: 'file-text',
        title: T('code_studio.panes.commit.empty_title'),
        message: T('code_studio.panes.commit.empty_body'),
      }));
    }
    filesSlot.replaceChildren(frag);
  }

  paint();

  return {
    update(data = {}) {
      if (disposed) return;
      if ('commit' in data) commit = data.commit;
      if ('patchSetId' in data && data.patchSetId) {
        call(ctx, 'codeStudioPatchSetGetRequest', { patchSetId: data.patchSetId })
          .then((body_) => {
            if (disposed) return;
            patchSet = body_.patch_set || null;
            files = (Array.isArray(body_.files) ? body_.files : [])
              .filter((f) => f.status === 'accepted' || f.status === 'partially_accepted');
            paint();
          })
          .catch((err) => failed(err, 'code_studio.panes.commit.load_failed'));
        return;
      }
      paint();
    },
    destroy() {
      disposed = true;
      hostEl.replaceChildren();
    },
  };
}

// ===========================================================================
// Dock: file tree
// ===========================================================================

export function renderFileTreeDock(hostEl, ctx) {
  hostEl.replaceChildren();
  let disposed = false;

  const title = dockTitle((ctx.workspace && ctx.workspace.name) || T('code_studio.panes.tree.title'));
  const tree = el('tf-tree', { variant: 'compact', lazy: '' });
  const truncatedHint = hint(T('code_studio.panes.tree.truncated'));
  truncatedHint.hidden = true;
  hostEl.append(title, tree, truncatedHint);

  const children = new Map(); // dir path ('' = root) -> node[]
  const expanded = new Set();
  let statusByPath = new Map();

  function badgeFor(path) {
    const mark = statusByPath.get(path);
    if (!mark) return null;
    const span = el('span', { class: `st ${mark.cls}`, text: mark.mark });
    return span;
  }

  function nodeFor(entry) {
    const name = entry.path.split('/').pop();
    const node = {
      id: entry.path,
      label: name,
      hasChildren: entry.kind === 'dir',
    };
    const badge = badgeFor(entry.path);
    if (badge) node.icon = badge;
    return node;
  }

  function buildNodes(dir) {
    const entries = children.get(dir) || [];
    return entries.map((entry) => {
      const node = nodeFor(entry);
      if (entry.kind === 'dir' && expanded.has(entry.path) && children.has(entry.path)) {
        node.children = buildNodes(entry.path);
      }
      return node;
    });
  }

  function paint() {
    tree.nodes = buildNodes('');
    tree.expandedIds = [...expanded];
  }

  async function loadDir(path) {
    try {
      const body_ = await call(ctx, 'codeStudioFileTreeRequest', { path, depth: 1 });
      if (disposed) return;
      children.set(path, Array.isArray(body_.entries) ? body_.entries : []);
      if (body_.truncated) truncatedHint.hidden = false;
      paint();
    } catch (err) {
      failed(err, 'code_studio.panes.tree.load_failed');
    }
  }

  async function loadStatus() {
    try {
      const body_ = await call(ctx, 'codeStudioGitStatusRequest', {});
      if (disposed) return;
      const map = new Map();
      for (const entry of body_.entries || []) {
        const code = `${entry.index_status || ''}${entry.worktree_status || ''}`;
        if (code.includes('U') || code.includes('!')) map.set(entry.path, CONFLICT_KIND);
        else if (code.includes('A') || code.includes('?')) map.set(entry.path, CHANGE_KIND.add);
        else if (code.includes('D')) map.set(entry.path, CHANGE_KIND.delete);
        else map.set(entry.path, CHANGE_KIND.modify);
      }
      statusByPath = map;
      paint();
    } catch (err) {
      failed(err, 'code_studio.panes.tree.status_failed');
    }
  }

  tree.addEventListener('expand', (e) => {
    const id = e.detail.id;
    expanded.add(id);
    if (children.has(id)) paint();
    else loadDir(id);
  });
  tree.addEventListener('collapse', (e) => {
    expanded.delete(e.detail.id);
    paint();
  });
  tree.addEventListener('select', (e) => {
    const id = e.detail.id;
    const isDir = (children.get(parentOf(id)) || []).some((x) => x.path === id && x.kind === 'dir');
    if (isDir) {
      if (expanded.has(id)) expanded.delete(id);
      else {
        expanded.add(id);
        if (!children.has(id)) loadDir(id);
      }
      paint();
      return;
    }
    tree.selectedId = id;
    // Opens the file as a stage tab; the shell switches the dock to "Pliki".
    ctx.openInStage('plik', id, T('code_studio.panes.tree.open_sub'));
  });

  function parentOf(path) {
    const idx = path.lastIndexOf('/');
    return idx === -1 ? '' : path.slice(0, idx);
  }

  loadDir('');
  loadStatus();

  return {
    update(data = {}) {
      if (disposed) return;
      if (data.refreshStatus) loadStatus();
      if (data.refreshPath != null) loadDir(data.refreshPath);
      if (data.selectedPath) tree.selectedId = data.selectedPath;
    },
    destroy() {
      disposed = true;
      hostEl.replaceChildren();
    },
  };
}

// ===========================================================================
// Dock: changes
// ===========================================================================

export function renderChangesDock(hostEl, ctx) {
  hostEl.replaceChildren();
  const bus = busFor(ctx);
  let disposed = false;

  const title = dockTitle('');
  const filesSlot = el('div', { class: 'cs-dock-files' });
  const decisionsTitle = dockTitle(T('code_studio.panes.changes.decisions_title'));
  const legend = el('div', { class: 'cs-legend' });
  const actions = el('div', { class: 'cs-actions' });
  const rejectRest = button({ variant: 'secondary', onClick: () => onRejectRest() },
    T('code_studio.panes.changes.reject_rest'));
  const confirm = button({ variant: 'primary', icon: 'check', onClick: () => onConfirm() },
    T('code_studio.panes.changes.confirm'));
  actions.append(rejectRest, confirm);
  hostEl.append(title, filesSlot, decisionsTitle, legend, actions,
    hint(T('code_studio.panes.changes.blob_origin')));

  function paintHeader() {
    const stats = setStats(bus.files);
    title.textContent = T('code_studio.panes.changes.dock_title', {
      files: bus.files.length,
      count: bus.files.length,
      add: stats.add,
      del: stats.del,
    });
    // The tab badge is this list's own count, so it is published from here.
    ctx.onReviewCount(bus.files.length);
  }

  function paintLegend() {
    const tally = tallyDecisions(bus);
    legend.replaceChildren(
      el('span', {}, el('span', { class: 'cs-dot ok' }),
        document.createTextNode(T('code_studio.panes.changes.legend_accepted', { count: tally.accepted }))),
      el('span', {}, el('span', { class: 'cs-dot err' }),
        document.createTextNode(T('code_studio.panes.changes.legend_rejected', { count: tally.rejected }))),
      el('span', {}, el('span', { class: 'cs-dot idle' }),
        document.createTextNode(T('code_studio.panes.changes.legend_pending', { count: tally.pending }))),
    );
  }

  function paintFiles() {
    const frag = document.createDocumentFragment();
    for (const file of bus.files) {
      const kind = kindOf(file);
      const stat = fileStats(file);
      const trailing = file.status === 'conflicted'
        ? el('span', { class: 'n', text: T('code_studio.panes.change.conflict') })
        : counts(stat.add, stat.del);
      frag.appendChild(
        rowButton(
          {
            tone: file.status === 'conflicted' ? 'err' : null,
            onClick: () => {
              bus.selectedFileId = file.patch_file_id;
              busEmit(bus, 'select');
              ctx.openInStage('zmiany', file.path, T(kind.labelKey), {
                patchSetId: bus.patchSet ? bus.patchSet.patch_set_id : '',
                patchFileId: file.patch_file_id,
              });
            },
          },
          statusMark(kind),
          el('span', { class: 'cs-row-label', text: file.path.split('/').pop() }),
          trailing,
        ),
      );
    }
    if (!bus.files.length) {
      frag.appendChild(el('tf-empty-state', {
        icon: 'code',
        title: T('code_studio.panes.changes.empty_title'),
        message: T('code_studio.panes.changes.empty_body'),
      }));
    }
    filesSlot.replaceChildren(frag);
  }

  function paintAll() {
    paintHeader();
    paintFiles();
    paintLegend();
  }

  const unsubscribe = busSubscribe(bus, (topic) => {
    if (disposed) return;
    if (topic === 'decisions') paintLegend();
    else paintAll();
  });

  function onRejectRest() {
    for (const file of bus.files) {
      for (const hunk of file.hunks || []) {
        if (!decisionOf(bus, hunk)) bus.decisions.set(hunk.patch_hunk_id, 'reject');
      }
    }
    paintLegend();
    busEmit(bus, 'decisions');
  }

  async function onConfirm() {
    if (!bus.patchSet) return;
    try {
      await saveDecisions(ctx, bus);
    } catch (err) {
      failed(err, 'code_studio.panes.changes.decide_failed');
    }
  }

  paintAll();

  return {
    update(data = {}) {
      if (disposed) return;
      if (data.patchSetId && (!bus.patchSet || bus.patchSet.patch_set_id !== data.patchSetId)) {
        loadPatchSet(ctx, bus, data.patchSetId)
          .catch((err) => failed(err, 'code_studio.panes.changes.load_failed'));
        return;
      }
      // The session hands over the whole list; the navigator opens the newest
      // set on its own, so entering a session shows the changes instead of an
      // empty column until someone clicks a patch card in the stream.
      if (!bus.patchSet && Array.isArray(data.patchSets) && data.patchSets.length) {
        const newest = [...data.patchSets]
          .sort((a, b) => String(b.created_at).localeCompare(String(a.created_at)))[0];
        if (newest) {
          loadPatchSet(ctx, bus, newest.patch_set_id)
            .catch((err) => failed(err, 'code_studio.panes.changes.load_failed'));
        }
      }
    },
    destroy() {
      disposed = true;
      unsubscribe();
      hostEl.replaceChildren();
    },
  };
}

// ===========================================================================
// Dock: git
// ===========================================================================

export function renderGitDock(hostEl, ctx) {
  hostEl.replaceChildren();
  let disposed = false;

  const branchRow = el('div', { class: 'git-branch' });
  const worktreesTitle = dockTitle(T('code_studio.panes.git.worktrees_title'));
  const worktreesSlot = el('div', { class: 'cs-dock-worktrees' });
  const historyTitle = dockTitle(T('code_studio.panes.git.history_title'));
  const historySlot = el('div', { class: 'cs-dock-history' });
  const actions = el('div', { class: 'cs-actions' });
  const fetchBtn = button({ variant: 'secondary', icon: 'refresh', onClick: () => onFetch() },
    T('code_studio.panes.git.fetch'));
  const pushBtn = button({ variant: 'primary', icon: 'branch', onClick: () => askPush() },
    T('code_studio.panes.git.push'));
  actions.append(fetchBtn, pushBtn);
  hostEl.append(branchRow, worktreesTitle, worktreesSlot, historyTitle, historySlot, actions,
    hint(T('code_studio.panes.git.always_asks')));

  function paintBranch(status) {
    branchRow.replaceChildren(
      icon('branch'),
      el('span', { class: 'b', text: status.branch || T('code_studio.panes.git.branch_unknown') }),
      el('span', {
        class: 'ahead',
        text: `↑${Number(status.ahead || 0)} ↓${Number(status.behind || 0)}`,
      }),
    );
  }

  // The list is what the session still HAS. Every finished merge leaves a
  // `removed` row behind, so an unfiltered list grows one dead integration tree
  // per attempt and buries the single live one among them.
  function paintWorktrees(all) {
    const worktrees = all.filter(isLiveWorktree);
    const historic = all.length - worktrees.length;
    const frag = document.createDocumentFragment();
    for (const wt of worktrees) {
      const held = wt.state === 'held';
      const row = el('div', { class: `wt${held ? ' held' : ''}` });
      const label = wt.purpose === 'integration'
        ? T('code_studio.panes.git.wt_integration')
        : T('code_studio.panes.git.wt_work');
      const sub = wt.branch
        ? T('code_studio.panes.git.wt_sub_branch', { kind: label, branch: wt.branch })
        : T('code_studio.panes.git.wt_sub_detached', { kind: label, base: shortOid(wt.base_commit) });
      row.append(
        icon(wt.purpose === 'integration' ? 'grid-rows' : 'folder'),
        el('span', { class: 'wt-txt' },
          el('span', { class: 'nm', text: worktreeName(wt) }),
          el('span', { class: 'sub', text: sub })),
      );
      if (held) row.appendChild(el('span', { class: 'wt-state', text: T('code_studio.panes.git.wt_held') }));
      frag.appendChild(row);
    }
    if (!worktrees.length) {
      frag.appendChild(hint(T('code_studio.panes.git.wt_empty')));
    }
    if (historic > 0) {
      frag.appendChild(hint(T('code_studio.panes.git.wt_historic', { count: historic })));
    }
    worktreesSlot.replaceChildren(frag);
  }

  function paintHistory(commits) {
    const frag = document.createDocumentFragment();
    commits.forEach((commit, idx) => {
      const row = el('div', { class: `git-commit${idx === 0 ? ' head' : ''}` });
      const node = el('span', { class: 'gnode' }, el('span', { class: 'gdot' }));
      if (idx < commits.length - 1) node.appendChild(el('span', { class: 'gline' }));
      row.append(
        node,
        el('span', { class: 'gtxt' },
          el('span', { class: 'gmsg', text: commit.subject }),
          el('span', {
            class: 'gmeta',
            text: `${shortOid(commit.oid || commit.short_oid)} · ${relDate(commit.date)}`,
          })),
      );
      frag.appendChild(row);
    });
    if (!commits.length) frag.appendChild(hint(T('code_studio.panes.git.history_empty')));
    historySlot.replaceChildren(frag);
  }

  async function refresh() {
    try {
      const [status, worktrees, log] = await Promise.all([
        call(ctx, 'codeStudioGitStatusRequest', {}),
        call(ctx, 'codeStudioWorktreesListRequest', {}),
        call(ctx, 'codeStudioGitLogRequest', { limit: 20 }),
      ]);
      if (disposed) return;
      paintBranch(status);
      paintWorktrees(Array.isArray(worktrees.worktrees) ? worktrees.worktrees : []);
      paintHistory(Array.isArray(log.commits) ? log.commits : []);
    } catch (err) {
      failed(err, 'code_studio.panes.git.load_failed');
    }
  }

  async function onFetch() {
    try {
      const body_ = await call(ctx, 'codeStudioGitSyncRequest', { mode: 'fetch' });
      if (disposed) return;
      toast(T('code_studio.panes.git.fetched', {
        ahead: Number(body_.ahead || 0),
        behind: Number(body_.behind || 0),
      }), 'success');
      refresh();
    } catch (err) {
      failed(err, 'code_studio.panes.git.fetch_failed');
    }
  }

  // Push is mandatory_interactive too — the button raises the question, it does
  // not push.
  function askPush() {
    ctx.ask({
      capability: 'git_push',
      mandatoryInteractive: true,
      summary: T('code_studio.panes.git.ask_push_summary'),
      detail: T('code_studio.panes.git.ask_push_detail'),
      request: {
        kind: 'codeStudioGitPushRequest',
        // An empty remote means "the workspace repository": the broker pushes by
        // URL, and a name like `origin` would be taken for one. It runs with an
        // isolated configuration (§11.2), so an upstream cannot be recorded and
        // asking for one is refused outright.
        payload: { remote: '', setUpstream: false },
      },
    });
  }

  refresh();

  return {
    update(data = {}) {
      if (disposed) return;
      if (data.refresh) refresh();
    },
    destroy() {
      disposed = true;
      hostEl.replaceChildren();
    },
  };
}

// ===========================================================================
// Dock: terminal sessions
// ===========================================================================

export function renderTerminalDock(hostEl, ctx) {
  hostEl.replaceChildren();
  let disposed = false;
  let sessions = [];
  let activeId = null;

  const title = dockTitle(T('code_studio.panes.terminal.dock_title'));
  const listSlot = el('div', { class: 'cs-dock-terminals' });
  const newBtn = button({ variant: 'secondary', icon: 'plus', onClick: () => openNew() },
    T('code_studio.panes.terminal.new'));
  hostEl.append(
    title,
    listSlot,
    newBtn,
    hint(T('code_studio.panes.terminal.sandbox_hint')),
    hint(T('code_studio.panes.terminal.no_git_metadata')),
  );

  function paint() {
    const frag = document.createDocumentFragment();
    for (const item of sessions) {
      frag.appendChild(
        rowButton(
          {
            active: item.terminal_id === activeId,
            onClick: () => {
              activeId = item.terminal_id;
              paint();
              ctx.openInStage('terminal', item.terminal_id,
                T('code_studio.panes.terminal.profile', {
                  mount: item.mount_access || 'cow',
                  network: item.network_access || 'none',
                }),
                { label: item.title || T('code_studio.panes.terminal.shell') });
            },
          },
          icon('code'),
          el('span', { class: 'cs-row-label', text: item.title || item.terminal_id }),
          el('span', { class: 'n', text: terminalStateLabel(item.state) }),
        ),
      );
    }
    if (!sessions.length) {
      frag.appendChild(hint(T('code_studio.panes.terminal.dock_empty')));
    }
    listSlot.replaceChildren(frag);
  }

  async function openNew() {
    try {
      const body_ = await call(ctx, 'codeStudioTerminalOpenRequest', { rows: 24, cols: 80 });
      if (disposed) return;
      sessions = [
        ...sessions,
        {
          terminal_id: body_.terminal_id,
          title: T('code_studio.panes.terminal.shell'),
          state: 'idle',
          mount_access: body_.mount_access,
          network_access: body_.network_access,
        },
      ];
      activeId = body_.terminal_id;
      paint();
      ctx.openInStage('terminal', body_.terminal_id,
        T('code_studio.panes.terminal.profile', {
          mount: body_.mount_access || 'cow',
          network: body_.network_access || 'none',
        }),
        { label: T('code_studio.panes.terminal.shell') });
    } catch (err) {
      failed(err, 'code_studio.panes.terminal.open_failed');
    }
  }

  paint();

  return {
    update(data = {}) {
      if (disposed) return;
      if (Array.isArray(data.terminals)) sessions = data.terminals;
      if (data.activeTerminalId) activeId = data.activeTerminalId;
      paint();
    },
    destroy() {
      disposed = true;
      hostEl.replaceChildren();
    },
  };
}

// ---------------------------------------------------------------------------
// Pane skeleton shared by every stage pane
// ---------------------------------------------------------------------------

function paneShell(hostEl, { withFoot = false } = {}) {
  hostEl.replaceChildren();
  const head = el('div', { class: 'cs-pane-head' });
  const body = el('div', { class: 'cs-pane-body' });
  hostEl.append(head, body);
  let foot = null;
  if (withFoot) {
    foot = el('div', { class: 'cs-pane-foot' });
    hostEl.appendChild(foot);
  }
  return { head, body, foot };
}
