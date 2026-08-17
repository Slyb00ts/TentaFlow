// ===== File: code-studio-session.js — Code Studio session console (K01) =====
//
// The inside of `.cs-body`: the stage (tab strip + console / file / changes /
// git / terminal / commit / sub-agent / inspector panes) and the dock
// (navigator lists). The shell around it — `.cs-shell`, the workspace strip,
// the sheet and `.cs-mtop` — belongs to code-studio.js; the phone chrome
// (bottom bar, back-to-chat shortcut, navigator scrim) is appended to that
// shell here and removed again on unmount.
//
// The timeline is the source of truth (plan §13.3): the stream is replayed from
// `codeStudioSessionTimeline` with an `after_seq` cursor, and a new event
// APPENDS a node — the stream is never re-rendered, so scroll position,
// expanded tool results and typed text survive.
//
// Everything travels over the binary protocol (ApiBinary + codec codeStudio*);
// there is no REST call and no fetch in this module.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-agent-activity.js';
import '/js/components/tf-button.js';
import '/js/components/tf-select.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-input.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-tooltip.js';

import {
  renderFilePane, renderChangesPane, renderGitPane, renderTerminalPane, renderCommitPane,
  renderFileTreeDock, renderChangesDock, renderGitDock, renderTerminalDock,
} from './code-studio-panes.js';

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

// Dock categories, in strip order. `stage` is the pane a category falls back to
// when it is selected and nothing of it is open yet.
const DOCK_CATEGORIES = [
  { id: 'agenci', icon: 'brain', stage: 'runs' },
  { id: 'pliki', icon: 'file-text', stage: 'plik' },
  { id: 'zmiany', icon: 'code', stage: 'zmiany' },
  { id: 'git', icon: 'branch', stage: 'git' },
  { id: 'terminal', icon: 'desktop', stage: 'terminal' },
];

// Phone bottom bar: six entries, each switching the view ABOVE itself.
const PHONE_VIEWS = [
  { id: 'konsola', icon: 'message' },
  { id: 'agenci', icon: 'brain' },
  { id: 'zmiany', icon: 'code' },
  { id: 'pliki', icon: 'file-text' },
  { id: 'git', icon: 'branch' },
  { id: 'terminal', icon: 'desktop' },
];

// view -> { stage, dock }. Mirrors the K01 script: a content view shows the
// stage, a list view shows the dock, and the bar drives both at once.
const VIEW_MAP = {
  konsola: { stage: 'konsola', dock: 'agenci' },
  agenci: { stage: 'subagent', dock: 'agenci' },
  pliki: { stage: 'plik', dock: 'pliki' },
  zmiany: { stage: 'zmiany', dock: 'zmiany' },
  git: { stage: 'git', dock: 'git' },
  terminal: { stage: 'terminal', dock: 'terminal' },
};

// Autonomy modes, weakest first. A session may be lowered freely and raised
// only up to the workspace ceiling; the server clamps regardless (§9.5).
const AUTONOMY_ORDER = ['plan', 'normal', 'auto_edit', 'autonomous'];

// Permission scopes offered for a plain approval (§9.1). `always` is refused
// server-side for mandatory-interactive capabilities, so it is shown inert with
// an explanation rather than hidden.
const APPROVAL_SCOPES = ['allow_once', 'allow_for_run', 'allow_for_session', 'always'];

// Capabilities whose question can never be switched off (§9.3 step 5).
const MANDATORY_CAPABILITIES = new Set(['git_push', 'git_merge', 'git_merge_finalize', 'secret_manage']);

// Capability that asks to run tests on the real worktree because a copy failed.
const DEGRADE_CAPABILITY = 'profile_degrade_rw';

// Capabilities whose approval IS the review gate 5a — decided in the changes
// pane (permission != review), never with a permission scope.
const REVIEW_CAPABILITIES = new Set(['git_commit', 'git_merge_finalize']);

// Tool name -> sprite id. Only ids that exist in the index.html sprite.
const TOOL_ICONS = [
  [/^(core\.)?fs_(read|list|glob)/, 'file-text'],
  [/^(core\.)?(fs_grep|code_search)/, 'search'],
  [/^(core\.)?fs_(write|edit|create|mkdir)/, 'edit'],
  [/^(core\.)?fs_(delete|remove)/, 'trash'],
  [/^(core\.)?fs_(rename|move)/, 'arrow'],
  [/^(core\.)?(exec|terminal)/, 'code'],
  [/^(core\.)?git_/, 'branch'],
  [/^(core\.)?agent_/, 'brain'],
  [/^(core\.)?ask_user/, 'message'],
  [/^(core\.)?net_/, 'globe'],
];

// change_kind / patch status -> the single letter of the status dictionary.
const CHANGE_LETTER = { add: 'a', modify: 'm', delete: 'd', rename: 'm' };

// The three tab strips are one component (<tf-tabs variant="bar">), so every
// strip needs its own id namespace: a tab id is a DOM id and `konsola` exists in
// two of them. The scene strip numbers its cells because a tab key is a path
// (`file:src/a b.rs`) and a path is not a DOM id.
const STAGE_HOME_ID = 'cs-st-home';
const DOCK_TAB_ID = (category) => `cs-dk-${category}`;
const VIEW_TAB_ID = (view) => `cs-mv-${view}`;

// Status dictionary -> tf-tab tone. The dot keeps the meaning it has in the
// legend; the letter keeps the colour of the `.st` badge in the file lists.
const DOT_TONES = { run: 'accent', ask: 'warn', ok: 'ok', err: 'err', idle: 'muted' };
const LETTER_TONES = { a: 'ok', m: 'warn', d: 'err', c: 'err' };

const TIMELINE_PAGE = 200;
const EXEC_PAGE = 500;
const TIMELINE_POLL_MS = 2500;
const SIDE_POLL_TICKS = 4; // every 4th timeline tick refreshes runs/ops/grants
const EVENT_BUFFER = 2000;

// ---------------------------------------------------------------------------
// Module state
// ---------------------------------------------------------------------------

let ctx = null;
let host = null;
let shell = null;
let state = null;

function freshState() {
  return {
    cursor: 0,
    seen: new Set(),
    events: [],
    eventsByRun: new Map(),
    subagentRuns: new Set(),
    toolCalls: new Map(), // call_id -> { name, node }
    execs: new Map(), // exec_id -> what its timeline row knows about the command
    exec: null, // the command whose output the exec pane is reading
    turnOrdinal: 0,
    runs: [],
    operations: [],
    grants: [],
    approvals: [],
    tasks: [],
    tasksOpen: 0,
    patchSets: [],
    reviewCount: 0,
    ask: null,
    paneRequest: null,
    openRunId: '',
    profile: null,
    autonomy: '',
    tabs: { agenci: [], pliki: [], zmiany: [], git: [], terminal: [] },
    tabSeq: 0,
    panes: {},
    docks: {},
    widgets: {},
    timer: 0,
    tick: 0,
    busy: false,
    handlers: null,
    shellClick: null,
  };
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

function t(key, vars) {
  return I18n.t(`code_studio.${key}`, vars || null);
}

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function node(html) {
  const tpl = document.createElement('template');
  tpl.innerHTML = html.trim();
  return tpl.content.firstElementChild;
}

// Write an attribute only when it actually changes: a custom element re-renders
// on every setAttribute, even one that writes the same value.
function attr(el, name, value) {
  const next = value == null ? '' : String(value);
  if (!next) {
    if (el.hasAttribute(name)) el.removeAttribute(name);
  } else if (el.getAttribute(name) !== next) {
    el.setAttribute(name, next);
  }
}

function flag(el, name, on) {
  if (!!on === el.hasAttribute(name)) return;
  if (on) el.setAttribute(name, '');
  else el.removeAttribute(name);
}

// SQLite writes `CURRENT_TIMESTAMP` as a zoneless `YYYY-MM-DD HH:MM:SS` in UTC,
// and `new Date()` reads exactly that form as LOCAL time. The difference cancels
// out between two server timestamps, so it stayed invisible in a duration — but
// it shifts every printed clock by the viewer's offset, and it makes a run still
// in flight (measured against `Date.now()`) look hours old. Anything already
// carrying a zone is left to the platform parser.
const NAIVE_TS_RE = /^(\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}:\d{2})(\.\d+)?$/;

function parseAt(value) {
  const text = String(value || '').trim();
  if (!text) return NaN;
  const naive = NAIVE_TS_RE.exec(text);
  return Date.parse(naive ? `${naive[1]}T${naive[2]}${naive[3] || ''}Z` : text);
}

function clockOf(iso) {
  const ms = parseAt(iso);
  if (Number.isNaN(ms)) return '';
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function durationOf(fromIso, toIso) {
  const a = parseAt(fromIso);
  const b = toIso ? parseAt(toIso) : Date.now();
  if (Number.isNaN(a) || Number.isNaN(b) || b < a) return '';
  const secs = Math.round((b - a) / 1000);
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  return `${mins}m ${String(secs % 60).padStart(2, '0')}s`;
}

function shortId(value) {
  return String(value || '').slice(0, 8);
}

// A blob digest is 40 characters and an operation id is 64. Printed in full they
// wrap over two lines of a phone timeline and say nothing a reader can act on,
// so the row prints the seven characters `git log --oneline` prints and keeps
// the whole value in `title`. The lookarounds stop the run from being cut out of
// a hyphenated uuid, which is an identifier and not a digest.
const HASH_RE = /(?<![0-9a-f-])[0-9a-f]{12,}(?![0-9a-f-])/g;

function shortHash(value) {
  const text = String(value || '');
  return text.length > 8 ? `${text.slice(0, 7)}…` : text;
}

function shortenHashes(text) {
  return String(text || '').replace(HASH_RE, (hex) => `${hex.slice(0, 7)}…`);
}

// `OperationFinished.error` carries the English `Display` text of the server's
// `FsError` (code_studio/fs/mod.rs). That is a closed set of shapes, so the
// timeline states the reason in the interface language instead of pasting a
// tool's stderr into a Polish screen. Each entry returns the translation key and
// its variables; the raw text stays on the row as `title` and in the expandable
// detail, so nothing is hidden — only the row is readable.
const OPERATION_ERRORS = [
  [/^conflict: expected absent, found ([0-9a-f]+)$/,
    (m) => ['event.err_conflict_absent', { current: shortHash(m[1]) }]],
  [/^conflict: expected ([0-9a-f]+), found nothing$/,
    (m) => ['event.err_conflict_gone', { base: shortHash(m[1]) }]],
  [/^conflict: expected ([0-9a-f]+), found ([0-9a-f]+)$/,
    (m) => ['event.err_conflict_moved', { base: shortHash(m[1]), current: shortHash(m[2]) }]],
  [/^no such file or directory$/, () => ['event.err_not_found', {}]],
  [/^already exists$/, () => ['event.err_exists', {}]],
  [/^not a directory$/, () => ['event.err_not_a_dir', {}]],
  [/^is a directory$/, () => ['event.err_is_a_dir', {}]],
  [/^file is not valid UTF-8 text$/, () => ['event.err_not_text', {}]],
  [/^edit is ambiguous: (\d+) occurrences of .*/,
    (m) => ['event.err_edit_ambiguous', { count: Number(m[1]) }]],
  [/^edit target .* does not occur in the file$/, () => ['event.err_edit_missing', {}]],
  [/^(\d+) bytes exceeds the (\d+) byte limit$/,
    (m) => ['event.err_too_large', { size: m[1], limit: m[2] }]],
  [/^limit exceeded: (.+)$/, (m) => ['event.err_limit', { reason: m[1] }]],
  [/^refused: (.+)$/, (m) => ['event.err_denied', { reason: m[1] }]],
  [/^invalid path: (.+)$/, (m) => ['event.err_bad_path', { reason: m[1] }]],
  [/^invalid request: (.+)$/, (m) => ['event.err_bad_request', { reason: m[1] }]],
  [/^io error: (.+)$/, (m) => ['event.err_io', { reason: m[1] }]],
];

function operationFailure(error) {
  const raw = String(error || '').trim();
  if (!raw) return { key: 'event.err_unknown', vars: {}, raw: '' };
  for (const [pattern, build] of OPERATION_ERRORS) {
    const match = pattern.exec(raw);
    if (match) {
      const [key, vars] = build(match);
      return { key, vars, raw };
    }
  }
  // Git and exec operations report free text no dictionary can cover. Marking it
  // as a quotation from the tool keeps it from reading like our own sentence.
  return { key: 'event.err_quoted', vars: { message: raw }, raw };
}

// An `exit 0` from a command the PEP narrowed to a copy-on-write mount is not
// the success it looks like: the process wrote, succeeded, and the worktree
// never saw a byte of it. The event says so with two fields — what the caller
// ASKED for and whether the writes were dropped — and a row that carries them
// has to say it louder than a tone change, because the reader's whole question
// is "did this land?".
//
// Rows written by a server that had neither field carry neither, so their
// absence means "nothing to claim", never "writes were discarded".
function execVerdict(payload) {
  const p = payload || {};
  const exit = p.exit_code ?? p.exitCode;
  const discarded = !!(p.writes_discarded ?? p.writesDiscarded);
  const requested = String(p.requested_mount_access ?? p.requestedMountAccess ?? '');
  const failed = exit != null && Number(exit) !== 0;
  return {
    execId: String(p.op_id ?? p.opId ?? ''),
    discarded,
    requested,
    // A failure stays a failure; only a command that "succeeded" needs the
    // amber warning to stop it from reading as done.
    tone: exit == null ? 'run' : (failed ? 'err' : (discarded ? 'wait' : 'ok')),
    noteKey: requested ? 'exec.discarded_note' : 'exec.discarded_note_plain',
  };
}

// One `session_runs` row in the shape the activity widget hydrates from. The
// widget is host-agnostic and takes epoch milliseconds, so the server's zoneless
// UTC timestamps are resolved here, where their format is known. Token counters
// are absent on a server that predates §17.3 accounting — an absent counter is
// left at zero rather than guessed at.
function runInfoOf(run) {
  const started = parseAt(run.started_at ?? run.startedAt);
  const finished = parseAt(run.finished_at ?? run.finishedAt);
  const kind = String(run.kind || '');
  return {
    // The widget renders this as the row's name. A raw `agent_id` is a uuid on
    // the orchestrator, which says nothing; the run chain names a run by its
    // kind and its ordinal, and the two lists must not disagree.
    agent: [kind ? t(`run_kind.${kind}`) : '', run.ordinal ? `#${run.ordinal}` : ''].filter(Boolean).join(' '),
    status: run.status,
    parentRunId: run.parent_run_id ?? run.parentRunId ?? '',
    startedAt: Number.isNaN(started) ? 0 : started,
    finishedAt: Number.isNaN(finished) ? 0 : finished,
    promptTokens: run.prompt_tokens ?? run.promptTokens ?? 0,
    completionTokens: run.completion_tokens ?? run.completionTokens ?? 0,
    model: run.model || '',
  };
}

function toolIcon(name) {
  const found = TOOL_ICONS.find(([re]) => re.test(String(name || '')));
  return found ? found[1] : 'bolt';
}

// A tool call carries a string map; show the argument a human recognises.
function toolArgument(args) {
  if (!args || typeof args !== 'object') return '';
  const preferred = ['path', 'from_path', 'query', 'argv', 'target', 'message', 'branch', 'remote', 'url', 'question'];
  for (const key of preferred) {
    if (args[key]) return String(args[key]);
  }
  const entries = Object.entries(args).filter(([, v]) => v !== '' && v != null);
  return entries.slice(0, 2).map(([k, v]) => `${k}=${v}`).join(' · ');
}

function dotClassFor(status) {
  switch (String(status || '')) {
    case 'running': return 'run';
    case 'waiting':
    case 'waiting_user': return 'ask';
    case 'completed': return 'ok';
    case 'failed':
    case 'error': return 'err';
    default: return 'idle';
  }
}

function profileLabel(mount, network) {
  if (!mount && !network) return '';
  return `${mount || '—'} × ${network || '—'}`;
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

export function mountSession(hostEl, context) {
  unmountSession();
  if (!hostEl || !context) return;

  ctx = {
    workspaceId: context.workspaceId || context.workspace?.workspace_id || '',
    sessionId: context.sessionId || context.session?.session_id || '',
    workspace: context.workspace || {},
    session: context.session || {},
    onExit: typeof context.onExit === 'function' ? context.onExit : null,
  };
  host = hostEl;
  shell = hostEl.closest('.cs-shell');
  state = freshState();
  state.autonomy = ctx.session.autonomy_mode || ctx.session.autonomyMode || 'normal';

  buildStage();
  buildDock();
  buildPhoneChrome();
  mountExternalPanes();
  wireEvents();
  seedTabs();

  setDock('agenci');
  setStage('konsola');
  setView('konsola');

  void bootstrap();
  state.timer = window.setInterval(pollTick, TIMELINE_POLL_MS);
}

export function unmountSession() {
  if (!state) {
    ctx = null; host = null; shell = null;
    return;
  }
  if (state.timer) window.clearInterval(state.timer);
  if (host && state.handlers) {
    for (const [type, fn] of state.handlers) host.removeEventListener(type, fn);
  }
  if (shell && state.shellClick) shell.removeEventListener('click', state.shellClick);
  for (const pane of Object.values(state.panes)) {
    try { pane?.destroy?.(); } catch (err) { console.warn('[code-studio] pane destroy failed:', err); }
  }
  for (const dock of Object.values(state.docks)) {
    try { dock?.destroy?.(); } catch (err) { console.warn('[code-studio] dock destroy failed:', err); }
  }
  if (shell) {
    shell.querySelectorAll('[data-cs-session-chrome]').forEach((el) => el.remove());
    delete shell.dataset.ask;
    shell.classList.remove('nav-open');
  }
  if (host) host.innerHTML = '';
  state = null;
  ctx = null;
  host = null;
  shell = null;
}

/** One event from the session stream (or a whole timeline response). */
export function applyEvent(event) {
  if (!state || !event) return;
  const list = Array.isArray(event.events) ? event.events : [event];
  ingestEvents(list);
}

// ---------------------------------------------------------------------------
// Stage
// ---------------------------------------------------------------------------

function buildStage() {
  const ws = ctx.workspace || {};
  const session = ctx.session || {};
  const native = (ws.exec_mode || ws.execMode) === 'trusted_native';
  const unrestricted = (ws.egress_enforcement || ws.egressEnforcement) === 'unrestricted';

  const stage = node(`
    <div class="cs-stage">
      <div class="cs-stage-bar">
        <tf-button class="cs-stage-exit" variant="ghost" size="sm" icon="chevron-left"
          data-action="exit" title="${escapeAttr(t('session_exit'))}"
          aria-label="${escapeAttr(t('session_exit'))}"></tf-button>
        <tf-tabs class="cs-stage-tabs" variant="bar" indicator="top" data-stage-tabs
          value="${STAGE_HOME_ID}">
          <tf-tab id="${STAGE_HOME_ID}" icon="message" pinned
            label="${escapeAttr(t('stage.console'))}"></tf-tab>
        </tf-tabs>
      </div>

      <div class="cs-spane" data-spane="konsola">
        <div class="cs-stage-head">
          <span class="cs-dot idle" data-session-dot></span>
          <div class="cs-stage-id">
            <div class="cs-stage-title">${escapeHtml(session.title || t('session.untitled'))}</div>
            <div class="cs-stage-sub" data-session-sub>${escapeHtml(t('session.orchestrator'))}</div>
          </div>
          <div class="cs-stage-chips">
            <span class="cs-chip accent">${sprite('branch')}${escapeHtml(session.branch || '—')}</span>
            <span class="cs-chip" data-autonomy-chip>${sprite('shield')}${escapeHtml(t(`autonomy.${state.autonomy}`))}</span>
            ${native ? `<span class="cs-chip warn" title="${escapeAttr(t('native.tooltip'))}">${sprite('alert')}${escapeHtml(t('native.chip'))}</span>` : ''}
            ${unrestricted ? `<span class="cs-chip danger" title="${escapeAttr(t('egress.unrestricted_tooltip'))}">${sprite('globe')}${escapeHtml(t('egress.unrestricted'))}</span>` : ''}
            <span class="cs-chip" data-profile-chip hidden>${sprite('shield')}<span data-profile-text></span></span>
          </div>
          <span class="spacer"></span>
          <tf-tooltip text="${escapeAttr(t('flow.unavailable'))}">
            <tf-button size="sm" icon="workflow-app" disabled>${escapeHtml(t('flow.open'))}</tf-button>
          </tf-tooltip>
          <tf-button size="sm" variant="danger" icon="stop" data-action="cancel-session">${escapeHtml(t('session.stop'))}</tf-button>
        </div>

        <div class="cs-stream" data-stream="console"></div>

        <div class="cs-nowbar" data-nowbar hidden>
          <tf-agent-activity variant="chat" data-activity="now"></tf-agent-activity>
        </div>

        <div class="cs-composer" data-composer>
          <div class="cs-answer" data-answer hidden></div>
          <div class="cs-composer-box">
            <tf-textarea rows="2" autogrow data-input
              placeholder="${escapeAttr(t('composer.placeholder'))}"></tf-textarea>
            <div class="cs-composer-bar">
              <tf-select data-autonomy label="">${autonomyOptions()}</tf-select>
              <span class="spacer"></span>
              <span class="cs-hint-keys"><span class="kbd">&crarr;</span> ${escapeHtml(t('composer.send_hint'))}</span>
              <tf-button size="sm" variant="primary" icon="send" data-action="send">${escapeHtml(t('composer.send'))}</tf-button>
            </div>
          </div>
        </div>
      </div>

      <div class="cs-spane" data-spane="plik"></div>
      <div class="cs-spane" data-spane="zmiany"></div>
      <div class="cs-spane" data-spane="git"></div>
      <div class="cs-spane" data-spane="terminal"></div>
      <div class="cs-spane" data-spane="commit"></div>

      <div class="cs-spane" data-spane="subagent">
        <div class="cs-pane-head">
          <span class="cs-dot idle" data-sub-dot></span>
          <span>
            <strong class="cs-pane-strong" data-sub-title>—</strong>
            <span class="cs-stage-sub" data-sub-meta></span>
          </span>
          <span class="spacer"></span>
          <tf-button size="sm" icon="chevron-left" data-stage-go="konsola">${escapeHtml(t('stage.main_agent'))}</tf-button>
        </div>
        <div class="cs-stream" data-stream="subagent"></div>
        <div class="cs-nowbar" data-sub-nowbar hidden></div>
      </div>

      <div class="cs-spane" data-spane="exec">
        <div class="cs-pane-head">
          <span class="cs-dot idle" data-exec-dot></span>
          <span>
            <strong class="cs-pane-strong" data-exec-title>—</strong>
            <span class="cs-stage-sub" data-exec-meta></span>
          </span>
          <span class="spacer"></span>
          <tf-button size="sm" icon="refresh" data-action="exec-refresh">${escapeHtml(t('exec.refresh'))}</tf-button>
          <tf-button size="sm" icon="chevron-left" data-stage-go="konsola">${escapeHtml(t('stage.main_agent'))}</tf-button>
        </div>
        <div class="cs-exec-warn" data-exec-warn hidden></div>
        <div class="cs-term cs-exec-out" data-exec-out></div>
        <div class="cs-pane-foot">
          <span class="cs-stage-sub" data-exec-count></span>
          <span class="spacer"></span>
          <tf-button size="sm" icon="chevron-down" data-action="exec-more" hidden>${escapeHtml(t('exec.more'))}</tf-button>
        </div>
      </div>

      <div class="cs-spane" data-spane="runs">
        <div class="cs-pane-head">
          <span><strong class="cs-pane-strong">${escapeHtml(t('inspector.title'))}</strong>
            <span class="cs-stage-sub">${escapeHtml(t('inspector.subtitle'))}</span></span>
          <span class="spacer"></span>
          <tf-button size="sm" icon="refresh" data-action="refresh-inspector">${escapeHtml(t('inspector.refresh'))}</tf-button>
        </div>
        <div class="cs-pane-body">
          <div class="cs-insp">
            <div>
              <div class="cs-dock-title">${escapeHtml(t('inspector.runs'))}</div>
              <div data-runs-chain></div>
            </div>
            <div>
              <div class="cs-dock-title">${escapeHtml(t('inspector.operations'))}</div>
              <div data-operations></div>
            </div>
            <div>
              <div class="cs-dock-title">${escapeHtml(t('inspector.grants'))}</div>
              <div data-grants></div>
            </div>
          </div>
        </div>
      </div>
    </div>
  `);
  host.appendChild(stage);
}

function autonomyOptions() {
  const ceiling = ctx.workspace.autonomy_ceiling || ctx.workspace.autonomyCeiling || 'normal';
  const max = Math.max(0, AUTONOMY_ORDER.indexOf(ceiling));
  return AUTONOMY_ORDER.slice(0, max + 1).map((mode) => (
    `<option value="${escapeAttr(mode)}"${mode === state.autonomy ? ' selected' : ''}>${escapeHtml(t('composer.mode', {}))} ${escapeHtml(t(`autonomy.${mode}`))}</option>`
  )).join('');
}

// ---------------------------------------------------------------------------
// Dock — navigator lists only; opening anything happens in the stage
// ---------------------------------------------------------------------------

function buildDock() {
  const dock = node(`
    <div class="cs-dock">
      <tf-tabs class="cs-dock-tabs" variant="bar" layout="stacked" indicator="bottom" data-dock-tabs>
        ${DOCK_CATEGORIES.map((c) => `
          <tf-tab id="${DOCK_TAB_ID(c.id)}" icon="${c.icon}"
            panel="cs-dock-pane-${c.id}"
            label="${escapeAttr(t(`dock.${c.id}`))}"></tf-tab>`).join('')}
      </tf-tabs>
      <div class="cs-dock-body">
        <div class="cs-dock-pane" id="cs-dock-pane-agenci" data-pane="agenci">
          <!-- The PLAN, above the runs that work it. The build loop's gate
               refuses to finish while anything here is open, so an operator
               watching a session has to be able to see the same list the gate
               is checking — otherwise the loop looks like it is spinning for
               no reason. Hidden until a plan exists. -->
          <div class="cs-plan" data-plan hidden>
            <div class="cs-dock-title">
              ${escapeHtml(t('dock.plan'))}
              <span class="cs-plan-open" data-plan-open></span>
            </div>
            <ol class="cs-plan-list" data-plan-list></ol>
          </div>
          <div class="cs-dock-title">${escapeHtml(t('dock.agents_active'))}</div>
          <!-- Pinned to the run list: the dock is a navigator, and a session
               whose runs have all finished still has to show what it ran and
               what each run cost. The collapsed bar auto-hides, which is right
               for a chat but leaves this column blank. -->
          <tf-agent-activity variant="chat" level="tree" data-activity="dock"></tf-agent-activity>
          <div class="cs-empty" data-agents-empty>
            <p>${escapeHtml(t('dock.agents_empty'))}</p>
          </div>
          <div class="cs-legend">
            <span><span class="cs-dot run"></span>${escapeHtml(t('legend.run'))}</span>
            <span><span class="cs-dot ask"></span>${escapeHtml(t('legend.ask'))}</span>
            <span><span class="cs-dot ok"></span>${escapeHtml(t('legend.ok'))}</span>
            <span><span class="cs-dot idle"></span>${escapeHtml(t('legend.idle'))}</span>
          </div>
        </div>
        <div class="cs-dock-pane" id="cs-dock-pane-pliki" data-pane="pliki"></div>
        <div class="cs-dock-pane" id="cs-dock-pane-zmiany" data-pane="zmiany"></div>
        <div class="cs-dock-pane" id="cs-dock-pane-git" data-pane="git"></div>
        <div class="cs-dock-pane" id="cs-dock-pane-terminal" data-pane="terminal"></div>
      </div>
    </div>
  `);
  host.appendChild(dock);

  state.widgets.dock = dock.querySelector('[data-activity="dock"]');
  state.widgets.now = host.querySelector('[data-activity="now"]');
  for (const widget of [state.widgets.dock, state.widgets.now]) {
    if (widget) widget.labels = activityLabels();
  }
}

// The component is i18n-agnostic: the host hands it a flat label dict.
function activityLabels() {
  return {
    background_one: t('activity.background'),
    background_many: t('activity.background'),
    iteration: t('activity.iteration'),
    idle: t('activity.idle'),
    runs_title: t('activity.runs_title'),
    no_runs: t('activity.no_runs'),
    timeline_title: t('activity.timeline_title'),
    no_steps: t('activity.no_steps'),
    cancel: t('activity.cancel'),
    elapsed: t('activity.elapsed'),
    tokens: t('activity.tokens'),
    asks: t('activity.asks'),
    back: t('activity.back'),
    step_tool: t('activity.step_tool'),
    step_child: t('activity.step_child'),
    step_question: t('activity.step_question'),
    step_permission: t('activity.step_permission'),
    step_resolved: t('activity.step_resolved'),
    step_iteration: t('activity.iteration'),
    step_node: t('activity.step_node'),
    step_compaction: t('activity.step_compaction'),
    step_router: t('activity.step_router'),
  };
}

// The widget renders its own terse "no runs" line, so exactly one of the two
// empty states may be on screen: the dock's sentence explains what the list
// would hold, and it replaces the widget until there is something to list.
function paintDockEmpty() {
  const widget = state.widgets.dock;
  const empty = host.querySelector('[data-agents-empty]');
  const listed = (widget?.runCount || 0) > 0;
  if (widget) widget.hidden = !listed;
  if (empty) empty.hidden = listed;
}

// ---------------------------------------------------------------------------
// Phone chrome — lives in the shell, because the bottom bar is a shell row
// ---------------------------------------------------------------------------

function buildPhoneChrome() {
  if (!shell) return;
  const scrim = node('<div class="cs-nav-scrim" data-nav data-cs-session-chrome></div>');
  const back = node(`
    <tf-button class="cs-backchat" variant="ghost" icon="chevron-left"
      data-view-go="konsola" data-cs-session-chrome>${escapeHtml(t('stage.main_agent'))}</tf-button>
  `);
  // The bottom bar rides the top edge of the strip with its rule, because the
  // strip's own border sits on top too (`tf-tabs[safe-area]`).
  const tabs = node(`
    <tf-tabs class="cs-mtabs" variant="bar" layout="stacked" indicator="top" safe-area
      data-view-tabs data-cs-session-chrome>
      ${PHONE_VIEWS.map((v) => `
        <tf-tab id="${VIEW_TAB_ID(v.id)}" icon="${v.icon}"
          label="${escapeAttr(t(`dock.${v.id}`))}"></tf-tab>`).join('')}
    </tf-tabs>
  `);
  shell.appendChild(scrim);
  shell.appendChild(back);
  shell.appendChild(tabs);
}

// ---------------------------------------------------------------------------
// External panes (agent C). Mounted once; fed through their `update`.
// ---------------------------------------------------------------------------

function mountExternalPanes() {
  // The panes navigate and ask through the shell: they own their content, the
  // shell owns the stage, the tab strip and every mandatory-interactive
  // question. Handing them only the two ids left both calls undefined.
  const scope = {
    workspaceId: ctx.workspaceId,
    sessionId: ctx.sessionId,
    workspace: ctx.workspace,
    session: ctx.session,
    openInStage,
    ask: askFromPane,
    // The change list owns which patch set it shows, so it also owns the number
    // on the "Zmiany" tab — the shell only paints what the list reports.
    onReviewCount: (count) => {
      state.reviewCount = Number(count) || 0;
      updateCounters();
    },
  };
  const spane = (id) => host.querySelector(`.cs-spane[data-spane="${id}"]`);
  const dpane = (id) => host.querySelector(`.cs-dock-pane[data-pane="${id}"]`);

  state.panes.plik = renderFilePane(spane('plik'), scope);
  state.panes.zmiany = renderChangesPane(spane('zmiany'), scope);
  state.panes.git = renderGitPane(spane('git'), scope);
  state.panes.terminal = renderTerminalPane(spane('terminal'), scope);
  state.panes.commit = renderCommitPane(spane('commit'), scope);

  state.docks.pliki = renderFileTreeDock(dpane('pliki'), scope);
  state.docks.zmiany = renderChangesDock(dpane('zmiany'), scope);
  state.docks.git = renderGitDock(dpane('git'), scope);
  state.docks.terminal = renderTerminalDock(dpane('terminal'), scope);
}

// ---------------------------------------------------------------------------
// Stage / dock / view state — three attributes on `.cs-shell`
// ---------------------------------------------------------------------------

function setStage(stage) {
  if (!shell) return;
  shell.dataset.stage = stage;
  shell.classList.remove('nav-open');
  if (stage === 'konsola') shell.dataset.view = 'konsola';
  else if (stage === 'plik') shell.dataset.view = 'pliki';
  else if (stage === 'subagent' || stage === 'runs') shell.dataset.view = 'agenci';
  // A command transcript belongs to the terminal category — that is where its
  // tab lives, and the phone bar has no cell of its own to light up.
  else if (stage === 'exec') shell.dataset.view = 'terminal';
  else shell.dataset.view = stage;
  paintNav();
}

function setDock(dock) {
  if (!shell) return;
  shell.dataset.dock = dock;
  shell.classList.remove('nav-open');
  syncStageTabs(dock);
  const tabs = state.tabs[dock] || [];
  const active = tabs.find((tab) => tab.active) || tabs[0];
  if (active) activateTab(dock, active.id);
  else setStage(DOCK_CATEGORIES.find((c) => c.id === dock)?.stage || 'konsola');
  paintNav();
}

function setView(view) {
  if (!shell) return;
  const map = VIEW_MAP[view] || VIEW_MAP.konsola;
  shell.dataset.view = view;
  shell.dataset.stage = map.stage;
  shell.dataset.dock = map.dock;
  shell.classList.remove('nav-open');
  syncStageTabs(map.dock);
  paintNav();
}

function selectTab(strip, id) {
  if (!strip || strip.getAttribute('value') === id) return;
  strip.value = id;
}

function paintNav() {
  if (!shell) return;
  selectTab(host.querySelector('[data-dock-tabs]'), DOCK_TAB_ID(shell.dataset.dock));
  selectTab(shell.querySelector('[data-view-tabs]'), VIEW_TAB_ID(shell.dataset.view));
  paintStageStrip();
}

// ---------------------------------------------------------------------------
// Stage tab strip — the list of OPEN items of the current dock category
// ---------------------------------------------------------------------------

function stageStrip() {
  return host?.querySelector('[data-stage-tabs]') || null;
}

// Every cell of the strip except the pinned console entry.
function stageTabEls(strip) {
  return Array.from(strip.querySelectorAll('tf-tab')).filter((el) => el.id !== STAGE_HOME_ID);
}

// A tab key is a path or an id with separators, so it lives in a data attribute
// and the DOM id is a plain counter minted once per cell.
function stageTabEl(strip, key) {
  return stageTabEls(strip).find((el) => el.dataset.csTab === key) || null;
}

// One leading slot, filled by whatever state the tab carries: the run status
// dot, the file status letter, or the plain kind icon.
function paintStageTab(el, tab) {
  attr(el, 'label', tab.label);
  attr(el, 'sub', tab.sub);
  flag(el, 'mono', !!tab.mono);
  attr(el, 'close-label', I18n.t('common.close'));
  flag(el, 'closable', !tab.pinned);
  if (tab.dot) {
    flag(el, 'dot', true);
    attr(el, 'marker', '');
    attr(el, 'icon', '');
    attr(el, 'tone', DOT_TONES[tab.dot] || 'muted');
  } else if (tab.letter) {
    flag(el, 'dot', false);
    attr(el, 'marker', tab.letter.text);
    attr(el, 'icon', '');
    attr(el, 'tone', LETTER_TONES[tab.letter.cls] || 'warn');
  } else {
    flag(el, 'dot', false);
    attr(el, 'marker', '');
    attr(el, 'icon', tab.icon);
    attr(el, 'tone', '');
  }
}

// The strip lists the open items of the CURRENT dock category, so switching
// category replaces its contents — but a single opened, renamed or closed tab
// touches only its own cell: cells are matched by key and updated in place.
function syncStageTabs(category) {
  const strip = stageStrip();
  if (!strip || !shell || shell.dataset.dock !== category) return;
  const wanted = state.tabs[category] || [];
  const keys = new Set(wanted.map((tab) => tab.id));
  for (const el of stageTabEls(strip)) {
    if (!keys.has(el.dataset.csTab)) el.remove();
  }
  let anchor = strip.querySelector(`#${STAGE_HOME_ID}`);
  for (const tab of wanted) {
    let el = stageTabEl(strip, tab.id);
    if (!el) {
      el = document.createElement('tf-tab');
      el.id = `cs-st-${state.tabSeq += 1}`;
      el.dataset.csTab = tab.id;
      el.dataset.csCat = category;
    }
    paintStageTab(el, tab);
    if (anchor.nextElementSibling !== el) anchor.after(el);
    anchor = el;
  }
  paintStageStrip();
}

// Which cell is lit, and whether the way back to the conversation nags. Both
// are projections of the shell attributes — the strip holds no state of its own.
function paintStageStrip() {
  const strip = stageStrip();
  if (!strip || !shell) return;
  const home = strip.querySelector(`#${STAGE_HOME_ID}`);
  if (home) flag(home, 'nudge', shell.dataset.ask === '1');
  let value = STAGE_HOME_ID;
  if (shell.dataset.stage !== 'konsola') {
    const active = (state.tabs[shell.dataset.dock] || []).find((tab) => tab.active);
    const el = active ? stageTabEl(strip, active.id) : null;
    if (el) value = el.id;
  }
  selectTab(strip, value);
}

function openTab(category, tab) {
  const tabs = state.tabs[category];
  if (!tabs) return;
  let existing = tabs.find((entry) => entry.id === tab.id);
  if (!existing) {
    existing = { pinned: false, mono: true, ...tab };
    tabs.push(existing);
  } else {
    Object.assign(existing, tab);
  }
  activateTab(category, existing.id);
}

function activateTab(category, id) {
  const tabs = state.tabs[category] || [];
  let active = null;
  for (const tab of tabs) {
    tab.active = tab.id === id;
    if (tab.active) active = tab;
  }
  if (shell && active) shell.dataset.dock = category;
  syncStageTabs(category);
  if (!active) return;
  applyTabToPane(active);
  setStage(active.stage);
}

// The pane header takes its description from the clicked tab: one pane serves
// every open item of a category.
function applyTabToPane(tab) {
  const pane = host.querySelector(`.cs-spane[data-spane="${tab.stage}"]`);
  if (pane) {
    const title = pane.querySelector('[data-slot="title"]');
    const sub = pane.querySelector('[data-slot="sub"]');
    if (title && tab.title) title.textContent = tab.title;
    if (sub && tab.sub) sub.textContent = tab.sub;
  }
  const handle = state.panes[tab.stage];
  if (handle?.update && tab.data) handle.update({ ...tab.data, workspaceId: ctx.workspaceId, sessionId: ctx.sessionId });
  if (tab.stage === 'subagent') openSubagent(tab.data?.runId || tab.id);
  if (tab.stage === 'exec') openExec(tab.data?.execId || '');
}

function closeTab(category, id) {
  const tabs = state.tabs[category] || [];
  const index = tabs.findIndex((tab) => tab.id === id);
  if (index < 0) return;
  const wasActive = tabs[index].active;
  tabs.splice(index, 1);
  syncStageTabs(category);
  if (!wasActive) return;
  const next = tabs[Math.min(index, tabs.length - 1)];
  if (next) activateTab(category, next.id);
  else setStage(category === 'agenci' ? 'runs' : 'konsola');
}

// ---------------------------------------------------------------------------
// Event wiring
// ---------------------------------------------------------------------------

function wireEvents() {
  state.handlers = [
    ['click', onHostClick],
    // Composer: Enter sends, Shift+Enter breaks the line.
    ['tf-keydown', (e) => {
      if (!e.target.matches('[data-input]')) return;
      const detail = e.detail || {};
      if (detail.key === 'Enter' && !detail.shiftKey) {
        detail.original?.preventDefault();
        void sendComposer();
      }
    }],
    ['change', (e) => {
      if (e.target.matches('[data-autonomy]')) void setAutonomy(e.target.value);
    }],
    // The dock lists are agent C's; they announce a pick with a bubbling event.
    ['cs-open', (e) => openFromDock(e.detail || {})],
    ['cs-open-file', (e) => openFromDock({ kind: 'file', ...(e.detail || {}) })],
    ['cs-open-change', (e) => openFromDock({ kind: 'change', ...(e.detail || {}) })],
    ['cs-open-commit', (e) => openFromDock({ kind: 'commit', ...(e.detail || {}) })],
    ['cs-open-terminal', (e) => openFromDock({ kind: 'terminal', ...(e.detail || {}) })],
    ['agent-open-run', (e) => {
      const runId = e.detail?.runId;
      if (runId) openSubagentTab(runId);
    }],
    ['agent-cancel', (e) => {
      const runId = e.detail?.runId;
      if (runId) void cancelSession(runId);
    }],
  ];
  for (const [type, fn] of state.handlers) host.addEventListener(type, fn);

  // The strips talk in their own events. They are listened to on the elements
  // themselves — the phone bar lives in the shell, whose bubbling path also
  // carries the two strips inside the host, so a delegated listener could not
  // tell them apart. Both strips die with the nodes they hang on (`host` is
  // emptied, the chrome is removed), so there is nothing to unbind.
  const stage = stageStrip();
  if (stage) {
    stage.addEventListener('change', onStageTabChange);
    stage.addEventListener('tab-close', onStageTabClose);
  }
  host.querySelector('[data-dock-tabs]')?.addEventListener('change', (e) => {
    setDock(String(e.detail?.value || '').slice(DOCK_TAB_ID('').length));
  });
  shell?.querySelector('[data-view-tabs]')?.addEventListener('change', (e) => {
    setView(String(e.detail?.value || '').slice(VIEW_TAB_ID('').length));
  });

  state.shellClick = onShellClick;
  if (shell) shell.addEventListener('click', state.shellClick);
}

function stageTabById(id) {
  const strip = stageStrip();
  if (!strip || !id) return null;
  return stageTabEls(strip).find((el) => el.id === id) || null;
}

function onStageTabChange(e) {
  const value = String(e.detail?.value || '');
  if (!value || value === STAGE_HOME_ID) { setStage('konsola'); return; }
  const el = stageTabById(value);
  if (el) activateTab(el.dataset.csCat, el.dataset.csTab);
}

function onStageTabClose(e) {
  const el = stageTabById(String(e.detail?.id || ''));
  if (el) closeTab(el.dataset.csCat, el.dataset.csTab);
}

// Two categories have something to show before anything is opened: the run
// inspector and the branch overview. They are pinned, so they cannot be closed.
function seedTabs() {
  state.tabs.agenci.push({
    id: 'runs', stage: 'runs', pinned: true, mono: false, icon: 'list',
    label: t('inspector.tab'), title: t('inspector.title'), sub: '',
  });
  state.tabs.git.push({
    id: 'branch', stage: 'git', pinned: true, mono: false, icon: 'branch',
    label: t('git.tab'), title: ctx.session.branch || '', sub: '',
  });
}

function onHostClick(e) {
  const stageGo = e.target.closest('[data-stage-go]');
  if (stageGo) { setStage(stageGo.dataset.stageGo); return; }

  const nav = e.target.closest('[data-nav]');
  if (nav) { shell?.classList.toggle('nav-open'); return; }

  const anchor = e.target.closest('.ev-askmark');
  if (anchor) { focusAsk(); return; }

  const spawn = e.target.closest('.ev-spawn[data-run]');
  if (spawn) { openSubagentTab(spawn.dataset.run); return; }

  const patchFile = e.target.closest('.pf[data-patch-set]');
  if (patchFile) {
    openChangeTab(patchFile.dataset.patchSet, patchFile.dataset.path, patchFile.dataset.fileId);
    return;
  }

  // The transcript is opened from the row's own affordance, so the rest of the
  // row keeps expanding its detail as every other tool row does.
  const execOpen = e.target.closest('.t-go');
  if (execOpen) {
    const execId = execOpen.closest('[data-exec]')?.dataset.exec;
    if (execId) { openExecTab(execId); return; }
  }

  const toolRow = e.target.closest('.ev-tool[data-detail]');
  if (toolRow) { toggleToolDetail(toolRow); return; }

  const action = e.target.closest('[data-action]');
  if (action) { void runAction(action); }
}

function onShellClick(e) {
  const viewGo = e.target.closest('[data-view-go]');
  if (viewGo) { setView(viewGo.dataset.viewGo); return; }
  const nav = e.target.closest('.cs-nav-scrim');
  if (nav) shell.classList.remove('nav-open');
}

async function runAction(el) {
  const action = el.dataset.action;
  switch (action) {
    case 'send': await sendComposer(); break;
    case 'cancel-session': await cancelSession(''); break;
    case 'refresh-inspector': await Promise.all([loadRuns(), loadOperations(), loadGrants()]); break;
    case 'exec-refresh': await reloadExec(); break;
    case 'exec-more': await loadExecPage(); break;
    case 'answer-scope': await decideApproval(el.dataset.scope); break;
    case 'answer-text': await answerQuestion(el.dataset.scope); break;
    case 'answer-confirm': await sendPaneRequest(); break;
    case 'answer-deny':
      if (state.ask?.kind === 'confirm') { state.paneRequest = null; clearAsk(); }
      else await decideApproval('deny');
      break;
    case 'answer-review': openReviewFromAsk(); break;
    case 'open-changes': openChangeTab(el.dataset.patchSet, '', ''); break;
    case 'resolve-op': await resolveOperation(el.dataset.opId, el.dataset.resolution, el); break;
    case 'revoke-grant': await revokeGrant(el.dataset.capability, el.dataset.pattern); break;
    case 'answer-expand': toggleAnswerExpanded(); break;
    case 'exit': ctx.onExit?.(); break;
    default: break;
  }
}

// Opening from a dock list: the item lands in the STAGE, never in the 372 px
// column — code, a diff and a terminal do not fit there.
function openFromDock(detail) {
  const kind = detail.kind || '';
  if (kind === 'file') {
    const path = String(detail.path || '');
    if (!path) return;
    openTab('pliki', {
      id: `file:${path}`, stage: 'plik', icon: 'file-text',
      label: path.split('/').pop(), title: path, sub: detail.sub || '',
      data: { path },
    });
  } else if (kind === 'change') {
    openChangeTab(detail.patchSetId || detail.patch_set_id || '', detail.path || '', detail.patchFileId || detail.patch_file_id || '');
  } else if (kind === 'commit') {
    const oid = String(detail.oid || detail.commit || '');
    if (!oid) return;
    openTab('git', {
      id: `commit:${oid}`, stage: 'commit', icon: 'branch',
      label: oid.slice(0, 7), title: detail.subject || oid, sub: detail.sub || '',
      data: { oid },
    });
  } else if (kind === 'terminal') {
    const id = String(detail.terminalId || detail.terminal_id || detail.id || '');
    if (!id) return;
    openTab('terminal', {
      id: `term:${id}`, stage: 'terminal', icon: 'desktop',
      label: detail.label || detail.title || id.slice(0, 8),
      title: detail.title || detail.label || '', sub: detail.sub || '',
      data: { terminalId: id },
    });
  }
}

function openChangeTab(patchSetId, path, patchFileId, sub = '') {
  const file = state.patchSets
    .flatMap((set) => (set.files || []).map((f) => ({ set, f })))
    .find(({ set, f }) => set.patch_set_id === patchSetId && (!path || f.path === path));
  const changeKind = file?.f?.change_kind || 'modify';
  const conflicted = file?.f?.status === 'conflicted';
  const label = (path || file?.f?.path || patchSetId).split('/').pop();
  openTab('zmiany', {
    id: `change:${patchSetId}:${path || 'all'}`,
    stage: 'zmiany',
    letter: conflicted
      ? { cls: 'c', text: '!' }
      : { cls: CHANGE_LETTER[changeKind] || 'm', text: (CHANGE_LETTER[changeKind] || 'm').toUpperCase() },
    label,
    title: path || file?.f?.path || '',
    sub,
    data: { patchSetId, path, patchFileId },
  });
}

// The panes' way into the stage. A dock list knows WHAT was picked; which tab
// carries it, which pane renders it and how the strip is labelled is the
// shell's business.
function openInStage(stage, key, sub, extra = {}) {
  const id = String(key || '');
  if (!id) return;
  if (stage === 'plik') {
    openFromDock({ kind: 'file', path: id, sub });
  } else if (stage === 'zmiany') {
    openChangeTab(String(extra.patchSetId || ''), id, String(extra.patchFileId || ''), sub);
  } else if (stage === 'terminal') {
    openFromDock({ kind: 'terminal', terminalId: id, label: extra.label || id.slice(0, 8), sub });
  }
}

function openSubagentTab(runId) {
  const run = state.runs.find((r) => r.run_id === runId);
  openTab('agenci', {
    id: `run:${runId}`,
    stage: 'subagent',
    dot: dotClassFor(run?.status),
    mono: false,
    label: run?.agent_id || shortId(runId),
    title: run?.agent_id || shortId(runId),
    sub: run ? `${run.kind} · ${run.status}` : '',
    data: { runId },
  });
}

// ---------------------------------------------------------------------------
// Stream — replayed from events, appended one node at a time
// ---------------------------------------------------------------------------

function streamEl(scope) {
  return host.querySelector(`.cs-stream[data-stream="${scope}"]`);
}

function atBottom(el) {
  return el.scrollHeight - el.scrollTop - el.clientHeight < 48;
}

function appendNode(el, child) {
  if (!el || !child) return;
  const stick = atBottom(el);
  el.appendChild(child);
  if (stick) el.scrollTop = el.scrollHeight;
}

function normalizeEvent(raw) {
  if (!raw || typeof raw !== 'object') return null;
  const kind = String(raw.kind || '');
  if (!kind) return null;
  let payload = {};
  const encoded = raw.payload_json ?? raw.payloadJson;
  if (typeof encoded === 'string' && encoded) {
    try { payload = JSON.parse(encoded); } catch { payload = {}; }
  } else if (encoded && typeof encoded === 'object') {
    payload = encoded;
  }
  // serde tags an enum externally: {"ToolCall": { … }}. Unwrap only a genuine
  // variant envelope (PascalCase single key), never a payload that happens to
  // have one field.
  const keys = Object.keys(payload);
  if (keys.length === 1 && /^[A-Z]/.test(keys[0]) && payload[keys[0]] && typeof payload[keys[0]] === 'object') {
    payload = payload[keys[0]];
  }
  return {
    seq: Number(raw.seq ?? 0),
    id: String(raw.event_id ?? raw.eventId ?? ''),
    kind,
    runId: String(raw.run_id ?? raw.runId ?? ''),
    agentId: String(raw.agent_id ?? raw.agentId ?? ''),
    at: String(raw.created_at ?? raw.createdAt ?? ''),
    security: !!(raw.security_relevant ?? raw.securityRelevant),
    p: payload,
  };
}

function ingestEvents(list) {
  const refresh = new Set();
  for (const raw of list) {
    const ev = normalizeEvent(raw);
    if (!ev || !ev.seq || state.seen.has(ev.seq)) continue;
    state.seen.add(ev.seq);
    if (ev.seq > state.cursor) state.cursor = ev.seq;

    state.events.push(ev);
    if (state.events.length > EVENT_BUFFER) {
      const dropped = state.events.shift();
      state.seen.delete(dropped.seq);
    }
    if (ev.runId) {
      const bucket = state.eventsByRun.get(ev.runId) || [];
      bucket.push(ev);
      if (bucket.length > 500) bucket.shift();
      state.eventsByRun.set(ev.runId, bucket);
    }

    classifyRun(ev);
    feedActivity(ev);

    const forConsole = !ev.runId || !state.subagentRuns.has(ev.runId);
    if (forConsole) appendNode(streamEl('console'), buildEventNode(ev, 'console'));
    if (state.openRunId && ev.runId === state.openRunId) {
      appendNode(streamEl('subagent'), buildEventNode(ev, 'subagent'));
    }
    reactToEvent(ev, refresh);
  }
  if (refresh.size) void refreshSide(refresh);
  paintDockEmpty();
  updateCounters();
}

// A sub-agent (or CLI) run gets its own pane; its events stay out of the main
// console, which follows the orchestrator.
function classifyRun(ev) {
  if (ev.kind !== 'run_started') return;
  const kind = String(ev.p.kind || '');
  if (kind === 'subagent' || kind === 'cli') state.subagentRuns.add(ev.p.run_id || ev.runId);
}

function reactToEvent(ev, refresh) {
  switch (ev.kind) {
    case 'approval_requested':
      refresh.add('approvals');
      break;
    case 'approval_decided':
      if (state.ask?.approvalId === ev.p.approval_id) clearAsk();
      refresh.add('approvals');
      break;
    // A patch set exists because the agent WROTE something, so the working tree
    // on disk is no longer what the file pane listed when it mounted.
    case 'patch_set_opened':
      refresh.add('patchsets');
      refresh.add('files');
      break;
    case 'patch_decided':
      refresh.add('patchsets');
      refresh.add('files');
      break;
    case 'run_started':
    case 'run_finished':
      refresh.add('runs');
      break;
    case 'operation_started':
    case 'operation_finished':
    case 'operation_reconciled':
      refresh.add('operations');
      break;
    // A merge, a push or a commit moves the branch and the worktree list; the
    // git pane rebuilds its merge state from them.
    case 'git_op':
      refresh.add('git');
      break;
    case 'autonomy_changed':
      state.autonomy = String(ev.p.to || state.autonomy);
      paintAutonomy();
      break;
    default:
      break;
  }
}

async function refreshSide(kinds) {
  const jobs = [];
  if (kinds.has('approvals')) jobs.push(loadApprovals());
  if (kinds.has('runs')) jobs.push(loadRuns(), loadTasks());
  if (kinds.has('operations')) jobs.push(loadOperations());
  if (kinds.has('patchsets')) jobs.push(loadPatchSets());
  if (kinds.has('git')) {
    state.panes.git?.update?.({ refresh: true });
    state.docks.git?.update?.({ refresh: true });
    // A commit or a merge rewrites the tree on disk too.
    kinds.add('files');
  }
  // The file pane loads its root ONCE, when the session view is built. Without
  // this it shows the worktree as it looked before the agent touched anything,
  // for as long as the session stays open.
  if (kinds.has('files')) {
    state.docks.pliki?.update?.({ refreshPath: '', refreshStatus: true });
  }
  await Promise.allSettled(jobs);
}

function feedActivity(ev) {
  const widgets = [state.widgets.dock, state.widgets.now].filter(Boolean);
  if (!widgets.length) return;
  const runId = ev.runId || ev.p.run_id || '';
  if (!runId) return;
  for (const widget of widgets) {
    switch (ev.kind) {
      case 'run_started':
        widget.applyEvent({ kind: 'child_spawned', run_id: runId, agent: ev.agentId || ev.p.kind || '' });
        break;
      case 'run_finished':
        widget.setRunStatus(runId, ev.p.status || 'completed');
        break;
      case 'tool_call':
        widget.applyEvent({ kind: 'tool_call_started', run_id: runId, agent: ev.agentId, name: ev.p.tool || '' });
        break;
      case 'tool_result':
        widget.applyEvent({
          kind: 'tool_call_finished', run_id: runId, agent: ev.agentId,
          name: state.toolCalls.get(ev.p.call_id)?.name || '', status: ev.p.ok ? 'ok' : 'error',
        });
        break;
      case 'approval_requested':
        // The composer owns the answer, so the widget only carries the state —
        // feeding it `permission_request` would raise a second question card.
        widget.setRunStatus(runId, 'waiting_user');
        break;
      case 'approval_decided':
        widget.setRunStatus(runId, 'running');
        break;
      default:
        break;
    }
  }
}

function buildEventNode(ev, scope) {
  switch (ev.kind) {
    case 'run_started': return runStartedNode(ev, scope);
    case 'run_finished': return turnNode(t('event.run_finished', { status: ev.p.status || '' }), ev.at);
    case 'agent_message': return messageNode(ev);
    case 'tool_call': return toolCallNode(ev);
    case 'tool_result': return toolResultNode(ev);
    case 'approval_requested': return askMarkNode(ev);
    case 'approval_decided':
      return toolNode({
        state: ev.p.decision === 'deny' ? 'err' : 'ok', icon: 'shield',
        name: t('event.approval_decided'),
        arg: `${ev.p.decision || ''} · ${shortId(ev.p.decided_by)}`,
        argTitle: String(ev.p.decided_by || ''),
        meta: [clockOf(ev.at)],
      });
    case 'patch_set_opened': return patchCardNode(ev);
    case 'patch_decided':
      return toolNode({
        state: ev.p.decision === 'rejected' ? 'err' : 'ok', icon: 'check',
        name: t('event.patch_decided'), arg: String(ev.p.decision || ''),
        meta: [clockOf(ev.at)],
      });
    case 'exec': {
      const verdict = execVerdict(ev.p);
      const el = toolNode({
        state: verdict.tone,
        icon: 'code', name: 'exec', arg: (ev.p.argv || []).join(' '),
        pill: verdict.discarded ? t('exec.discarded_pill') : '',
        pillTone: verdict.discarded ? 'warn' : '',
        warn: verdict.discarded ? t(verdict.noteKey, { requested: verdict.requested }) : '',
        meta: [ev.p.exit_code == null ? '' : `exit ${ev.p.exit_code}`, clockOf(ev.at)],
        detail: ev.p.cwd ? `cwd: ${ev.p.cwd}` : '',
        go: verdict.execId ? t('exec.open') : '',
      });
      if (verdict.execId) {
        el.dataset.exec = verdict.execId;
        state.execs.set(verdict.execId, {
          execId: verdict.execId,
          argv: (ev.p.argv || []).map(String),
          cwd: String(ev.p.cwd || ''),
          exitCode: ev.p.exit_code ?? ev.p.exitCode ?? null,
          at: ev.at,
          verdict,
        });
      }
      return el;
    }
    case 'git_op':
      return toolNode({
        state: 'ok', icon: 'branch', name: `git ${String(ev.p.operation || '').toLowerCase()}`,
        arg: [ev.p.refname, ev.p.remote].filter(Boolean).join(' · '),
        meta: [shortHash(ev.p.new_oid), clockOf(ev.at)],
        detail: ev.p.old_oid ? `${ev.p.old_oid} → ${ev.p.new_oid || ''}` : '',
      });
    case 'egress':
      return toolNode({
        state: ev.p.allowed ? 'ok' : 'err', icon: 'globe', name: t('event.egress'),
        arg: String(ev.p.url || ''), meta: [clockOf(ev.at)], detail: String(ev.p.reason || ''),
      });
    case 'secret_access':
      return toolNode({
        state: 'wait', icon: 'lock', name: t('event.secret_access'),
        arg: String(ev.p.purpose || ''), meta: [clockOf(ev.at)],
      });
    case 'ticket_issued':
      return toolNode({
        state: 'ok', icon: 'key', name: t('event.ticket_issued'),
        arg: String(ev.p.engine_id || ''), meta: [`${ev.p.budget_tokens || 0} tok`, clockOf(ev.at)],
      });
    case 'sandbox':
      return toolNode({
        state: ev.p.state === 'failed' ? 'err' : 'ok', icon: 'shield', name: t('event.sandbox'),
        arg: `${ev.p.state || ''} · ${profileLabel(ev.p.mount_access, ev.p.network_access)}`,
        meta: [clockOf(ev.at)],
      });
    case 'autonomy_changed':
      return toolNode({
        state: 'wait', icon: 'shield', name: t('event.autonomy_changed'),
        arg: `${t(`autonomy.${ev.p.from}`)} → ${t(`autonomy.${ev.p.to}`)}`,
        meta: [ev.p.changed_by || '', clockOf(ev.at)],
      });
    case 'allowlist_changed':
      return toolNode({
        state: 'wait', icon: 'rules', name: t('event.allowlist_changed'),
        arg: [...(ev.p.added || []).map((x) => `+${x}`), ...(ev.p.removed || []).map((x) => `-${x}`)].join(' '),
        meta: [ev.p.changed_by || '', clockOf(ev.at)],
      });
    case 'member_added':
      return toolNode({
        state: 'ok', icon: 'users', name: t('event.member_added'),
        arg: `${ev.p.user_id || ''} · ${ev.p.role || ''}`, meta: [clockOf(ev.at)],
      });
    case 'workspace_created':
      return toolNode({
        state: 'ok', icon: 'folder', name: t('event.workspace_created'),
        arg: `${ev.p.exec_mode || ''} · ${ev.p.node_id || ''}`, meta: [clockOf(ev.at)],
      });
    case 'operation_reconciled':
      return toolNode({
        state: ev.p.to === 'unknown' ? 'wait' : 'ok', icon: 'refresh',
        name: t('event.operation_reconciled'),
        arg: `${ev.p.op_kind || ''}: ${ev.p.from || ''} → ${ev.p.to || ''}`,
        meta: [clockOf(ev.at)], detail: String(ev.p.reason || ''),
      });
    case 'projection_corrected':
      return toolNode({
        state: 'wait', icon: 'alert', name: t('event.projection_corrected'),
        arg: `${ev.p.entity || ''} ${shortId(ev.p.id)}`,
        meta: [clockOf(ev.at)],
        detail: `${ev.p.projected || ''} → ${ev.p.from_events || ''}`,
      });
    // operation_started / operation_finished are the journal, not the
    // conversation: they belong to the inspector list, and only a failure is
    // worth interrupting the stream for.
    case 'operation_finished': {
      if (ev.p.status !== 'failed') return null;
      // A failed operation reads as a tool RESULT — name, outcome, one sentence
      // of reason — not as the raw stderr the server happens to carry.
      const failure = operationFailure(ev.p.error);
      return toolNode({
        state: 'err', icon: 'alert', name: ev.p.op_kind || t('event.operation'),
        pill: t('op_status.failed'),
        arg: t(failure.key, failure.vars), argTitle: failure.raw,
        meta: [clockOf(ev.at)], detail: failure.raw,
      });
    }
    default:
      return null;
  }
}

function turnNode(label, at) {
  const clock = clockOf(at);
  return node(`<div class="cs-turn">${escapeHtml(label)}${clock ? ` · ${escapeHtml(clock)}` : ''}</div>`);
}

function runStartedNode(ev, scope) {
  const kind = String(ev.p.kind || 'root');
  const trigger = String(ev.p.trigger || '');
  if (scope === 'console' && (kind === 'subagent' || kind === 'cli')) {
    return node(`
      <div class="ev ev-spawn" data-run="${escapeAttr(ev.p.run_id || ev.runId)}">
        <span class="av">${sprite('brain')}</span>
        <span>
          <span class="nm">${escapeHtml(ev.agentId || shortId(ev.p.run_id || ev.runId))}</span>
          <span class="ds">${escapeHtml(t(`trigger.${trigger}`))}</span>
        </span>
        <span class="go">${escapeHtml(t('stage.see_run'))} ${sprite('chevron-right')}</span>
      </div>
    `);
  }
  // The ordinal counts turns of the MAIN conversation; re-rendering a
  // sub-agent pane must not advance it.
  if (scope === 'console') state.turnOrdinal += 1;
  const n = scope === 'console' ? state.turnOrdinal : Number(ev.p.ordinal || 1);
  if (kind === 'revision') {
    return turnNode(t('event.revision_turn', { n, reason: t(`trigger.${trigger}`) }), ev.at);
  }
  return turnNode(t('event.turn', { n }), ev.at);
}

function messageNode(ev) {
  const role = String(ev.p.role || 'assistant');
  const text = String(ev.p.text || '');
  if (role === 'user') {
    return node(`<div class="ev ev-user">${escapeHtml(text)}</div>`);
  }
  if (role === 'thinking' || role === 'reasoning') {
    return node(`<div class="ev ev-think">${escapeHtml(text)}</div>`);
  }
  return node(`<div class="ev ev-say">${escapeHtml(text)}</div>`);
}

// `state` is the tone of the row, `pill` the outcome word an operation result
// carries next to its name, `argTitle` the untruncated value behind an argument
// the row had to shorten, `warn` a full-width sentence the row must not let a
// reader miss, and `go` a trailing affordance that opens the row's own scene.
// Every argument passes `shortenHashes` here — one choke point is what keeps a
// 40-character digest from reaching the stream through a branch nobody
// remembered to guard.
//
// The warning is rendered INSIDE the row rather than as a sibling, because the
// row's expandable detail is its next sibling: a second node between them would
// make every second click stack another copy of the detail.
function toolNode({ state: tone, icon, name, pill, pillTone, arg, argTitle, meta, detail, warn, go }) {
  const full = String(arg || '');
  const text = shortenHashes(full);
  const title = argTitle || (text === full ? '' : full);
  const metaHtml = (meta || []).filter(Boolean)
    .map((entry) => `<span>${escapeHtml(entry)}</span>`).join('');
  const pillHtml = pill
    ? `<span class="t-state${pillTone ? ` ${escapeAttr(pillTone)}` : ''}">${escapeHtml(pill)}</span>`
    : '';
  const goHtml = go ? `<span class="t-go">${escapeHtml(go)}${sprite('chevron-right')}</span>` : '';
  const warnHtml = warn
    ? `<span class="t-warn">${sprite('alert')}${escapeHtml(warn)}</span>`
    : '';
  const el = node(`
    <div class="ev ev-tool ${escapeAttr(tone || 'ok')}${warn ? ' has-warn' : ''}"${detail ? ' data-detail="1"' : ''}>
      <span class="t-ico">${sprite(icon || 'bolt')}</span>
      <span class="t-name">${escapeHtml(name || '')}</span>
      ${pillHtml}
      <span class="t-arg"${title ? ` title="${escapeAttr(title)}"` : ''}>${escapeHtml(text)}</span>
      <span class="t-meta">${metaHtml}</span>
      ${goHtml}
      ${warnHtml}
    </div>
  `);
  if (detail) el.dataset.detailText = detail;
  return el;
}

function toolCallNode(ev) {
  const tool = String(ev.p.tool || '');
  const el = toolNode({
    state: 'run',
    icon: toolIcon(tool),
    name: tool.replace(/^core\./, ''),
    arg: toolArgument(ev.p.arguments),
    meta: [clockOf(ev.at)],
  });
  if (ev.p.call_id) state.toolCalls.set(ev.p.call_id, { name: tool, node: el, at: ev.at });
  if (/ask_user$/.test(tool)) {
    setAsk(questionFromToolCall(ev));
    return node(`
      <div class="ev ev-askmark">
        <span class="cs-dot ask"></span>
        <span><span class="q">${escapeHtml(t('ask.anchor'))}</span> ${escapeHtml(String(ev.p.arguments?.question || ev.p.arguments?.summary || ''))}</span>
        <span class="go">${escapeHtml(t('ask.go'))}</span>
      </div>
    `);
  }
  return el;
}

// The result flips the pending row in place — a fresh node would duplicate the
// call and break the "append only" contract of the stream.
function toolResultNode(ev) {
  const call = state.toolCalls.get(ev.p.call_id);
  if (!call || !call.node?.isConnected) {
    return toolNode({
      state: ev.p.ok ? 'ok' : 'err', icon: 'bolt', name: t('event.tool_result'),
      arg: String(ev.p.summary || ''), meta: [clockOf(ev.at)],
    });
  }
  const el = call.node;
  el.classList.remove('run');
  el.classList.add(ev.p.ok ? 'ok' : 'err');
  const meta = el.querySelector('.t-meta');
  if (meta) {
    // The clock stays: a duration alone dates nothing, and a timeline whose rows
    // lose their hour the moment they finish cannot be read back afterwards.
    meta.innerHTML = [clockOf(call.at), durationOf(call.at, ev.at)]
      .filter(Boolean).map((entry) => `<span>${escapeHtml(entry)}</span>`).join('')
      + (ev.p.ok ? `${sprite('check')}` : `${sprite('alert')}`);
  }
  if (ev.p.summary) {
    el.dataset.detail = '1';
    el.dataset.detailText = String(ev.p.summary);
  }
  return null;
}

function toggleToolDetail(row) {
  const next = row.nextElementSibling;
  if (next && next.classList.contains('ev-detail')) { next.remove(); return; }
  const detail = row.dataset.detailText || '';
  if (!detail) return;
  row.after(node(`<div class="ev ev-detail">${escapeHtml(detail)}</div>`));
}

function askMarkNode(ev) {
  return node(`
    <div class="ev ev-askmark" data-approval="${escapeAttr(ev.p.approval_id || '')}">
      <span class="cs-dot ask"></span>
      <span><span class="q">${escapeHtml(t('ask.anchor'))}</span> ${escapeHtml(String(ev.p.summary || ev.p.capability || ''))}</span>
      <span class="go">${escapeHtml(t('ask.go'))}</span>
    </div>
  `);
}

function patchCardNode(ev) {
  const patchSetId = String(ev.p.patch_set_id || '');
  const el = node(`
    <div class="ev ev-patch" data-patch-card="${escapeAttr(patchSetId)}">
      <div class="ev-patch-head">
        ${sprite('check')}
        ${escapeHtml(t('patch.title'))}
        <span class="sum">${escapeHtml(t('patch.files', { count: Number(ev.p.files || 0) }))}</span>
      </div>
      <div class="ev-patch-files" data-patch-files></div>
      <div class="ev-patch-foot">
        <tf-button size="sm" variant="primary" icon="eye" data-action="open-changes" data-patch-set="${escapeAttr(patchSetId)}">
          ${escapeHtml(t('patch.review'))}
        </tf-button>
      </div>
    </div>
  `);
  void fillPatchCard(patchSetId, el);
  return el;
}

async function fillPatchCard(patchSetId, card) {
  if (!patchSetId) return;
  try {
    const resp = await ApiBinary.one('codeStudioPatchSetGetRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, patchSetId,
    });
    const files = resp.files || [];
    upsertPatchSet({ patch_set_id: patchSetId, files, status: resp.patch_set?.status || 'open' });
    // `PatchSetOpened.files` is the number the event carried when the set was
    // opened; the set has since gained or lost files. The card lists what the
    // set holds NOW, so its headline has to be recounted from the same list.
    const sum = card.querySelector('.sum');
    if (sum) sum.textContent = t('patch.files', { count: files.length });
    const list = card.querySelector('[data-patch-files]');
    if (!list) return;
    list.innerHTML = files.map((file) => {
      const conflicted = file.status === 'conflicted';
      const cls = conflicted ? 'c' : (CHANGE_LETTER[file.change_kind] || 'm');
      const letter = conflicted ? '!' : cls.toUpperCase();
      return `
        <div class="pf" data-patch-set="${escapeAttr(patchSetId)}" data-path="${escapeAttr(file.path)}"
             data-file-id="${escapeAttr(file.patch_file_id || '')}">
          <span class="st ${cls}">${escapeHtml(letter)}</span>
          <span class="p">${escapeHtml(file.path)}</span>
          <span class="n">${escapeHtml(t(`patch.status.${file.status}`))}</span>
        </div>`;
    }).join('');
    updateCounters();
  } catch (err) {
    console.warn('[code-studio] patch set load failed:', err?.message ?? err);
  }
}

function upsertPatchSet(entry) {
  const index = state.patchSets.findIndex((set) => set.patch_set_id === entry.patch_set_id);
  if (index < 0) state.patchSets.push(entry);
  else state.patchSets[index] = { ...state.patchSets[index], ...entry };
  state.docks.zmiany?.update?.({
    workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, patchSets: state.patchSets,
  });
}

// ---------------------------------------------------------------------------
// Sub-agent pane
// ---------------------------------------------------------------------------

function openSubagent(runId) {
  if (!runId) return;
  state.openRunId = runId;
  const run = state.runs.find((r) => r.run_id === runId);
  const title = host.querySelector('[data-sub-title]');
  const meta = host.querySelector('[data-sub-meta]');
  const dot = host.querySelector('[data-sub-dot]');
  if (title) title.textContent = run?.agent_id || shortId(runId);
  if (meta) {
    meta.textContent = [
      run ? t(`run_kind.${run.kind}`) : '',
      run ? t(`trigger.${run.trigger}`) : '',
      run ? durationOf(run.started_at, run.finished_at) : '',
    ].filter(Boolean).join(' · ');
  }
  if (dot) dot.className = `cs-dot ${dotClassFor(run?.status)}`;

  const stream = streamEl('subagent');
  if (!stream) return;
  stream.innerHTML = '';
  const first = node(`<div class="cs-turn">${escapeHtml(t('stage.assignment'))}</div>`);
  stream.appendChild(first);
  for (const ev of state.eventsByRun.get(runId) || []) {
    const child = buildEventNode(ev, 'subagent');
    if (child) stream.appendChild(child);
  }
  stream.scrollTop = stream.scrollHeight;
}

// ---------------------------------------------------------------------------
// Exec pane — what a finished command printed
//
// `ExecStartResponse` says only that the command was accepted; its stdout and
// stderr are the artifact the operation was closed with, and until
// `codeStudioExecOutputRequest` existed no client could read them at all. The
// pane is a reader over that artifact: a cursor of line numbers, one page at a
// time, never re-rendering what is already on screen.
// ---------------------------------------------------------------------------

function openExecTab(execId) {
  const id = String(execId || '');
  if (!id) return;
  const info = state.execs.get(id) || {};
  const command = (info.argv || []).join(' ');
  openTab('terminal', {
    id: `exec:${id}`,
    stage: 'exec',
    icon: 'code',
    label: (info.argv || [])[0] || shortId(id),
    title: command || shortId(id),
    sub: info.cwd || '',
    data: { execId: id },
  });
}

function openExec(execId) {
  const id = String(execId || '');
  if (!id) return;
  if (!state.exec || state.exec.execId !== id) {
    state.exec = { execId: id, cursor: 0, count: 0, hasMore: false, status: '', loading: false, error: '' };
    const out = host.querySelector('[data-exec-out]');
    if (out) out.innerHTML = '';
  }
  paintExecHead();
  void loadExecPage();
}

async function reloadExec() {
  const view = state.exec;
  if (!view) return;
  const execId = view.execId;
  state.exec = null;
  openExec(execId);
}

function paintExecHead() {
  const view = state.exec;
  if (!view) return;
  const info = state.execs.get(view.execId) || {};
  const verdict = info.verdict || {};
  const title = host.querySelector('[data-exec-title]');
  const meta = host.querySelector('[data-exec-meta]');
  const dot = host.querySelector('[data-exec-dot]');
  const warn = host.querySelector('[data-exec-warn]');
  if (title) title.textContent = (info.argv || []).join(' ') || shortId(view.execId);
  if (meta) {
    meta.textContent = [
      info.exitCode == null ? '' : `exit ${info.exitCode}`,
      info.cwd || '',
      clockOf(info.at),
    ].filter(Boolean).join(' · ');
  }
  if (dot) {
    dot.className = `cs-dot ${info.exitCode == null ? 'run' : (Number(info.exitCode) === 0 ? 'ok' : 'err')}`;
  }
  if (warn) {
    warn.hidden = !verdict.discarded;
    warn.innerHTML = verdict.discarded
      ? `${sprite('alert')}<span><strong>${escapeHtml(t('exec.discarded_pill'))}</strong> ${escapeHtml(t(verdict.noteKey, { requested: verdict.requested }))}</span>`
      : '';
  }
}

// The server answers with the cursor it reached. Trusting it blindly would
// replay a page whenever it went backwards, and trusting only the line count
// would drift the moment a page is clamped — so the cursor only ever advances,
// and "more" is refused when a page moved nothing.
function mergeExecPage(view, resp) {
  const lines = Array.isArray(resp?.lines) ? resp.lines.map(String) : [];
  const answered = Number(resp?.next_seq ?? resp?.nextSeq ?? NaN);
  const cursor = Number.isFinite(answered) && answered > view.cursor
    ? answered
    : view.cursor + lines.length;
  return {
    lines,
    cursor,
    count: view.count + lines.length,
    hasMore: !!(resp?.has_more ?? resp?.hasMore) && cursor > view.cursor,
    status: String(resp?.status ?? view.status ?? ''),
  };
}

async function loadExecPage() {
  const view = state.exec;
  if (!view || view.loading) return;
  view.loading = true;
  paintExecFoot();
  try {
    const resp = await ApiBinary.one('codeStudioExecOutputRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId,
      execId: view.execId, afterSeq: view.cursor, limit: EXEC_PAGE,
    });
    const page = mergeExecPage(view, resp);
    appendExecLines(page.lines);
    view.cursor = page.cursor;
    view.count = page.count;
    view.hasMore = page.hasMore;
    view.status = page.status;
    view.error = '';
  } catch (err) {
    // A server that predates the request kind answers with a decode failure.
    // The pane says so where the reader is looking instead of only in a console
    // nobody has open.
    view.error = String(err?.message ?? err);
  } finally {
    view.loading = false;
    paintExecFoot();
  }
}

function appendExecLines(lines) {
  const out = host.querySelector('[data-exec-out]');
  if (!out || !lines.length) return;
  const stick = atBottom(out);
  const frag = document.createDocumentFragment();
  for (const line of lines) {
    const row = document.createElement('div');
    row.className = 'eo-line';
    row.textContent = line;
    frag.appendChild(row);
  }
  out.appendChild(frag);
  if (stick) out.scrollTop = out.scrollHeight;
}

function paintExecFoot() {
  const view = state.exec;
  const count = host.querySelector('[data-exec-count]');
  const more = host.querySelector('[data-action="exec-more"]');
  const out = host.querySelector('[data-exec-out]');
  if (!view || !count) return;
  if (view.error) count.textContent = t('exec.unavailable', { reason: view.error });
  else if (view.loading && !view.count) count.textContent = t('exec.loading');
  else if (view.count) count.textContent = t('exec.lines', { count: view.count });
  else if (view.status && view.status !== 'completed' && view.status !== 'failed') {
    count.textContent = t('exec.still_running');
  } else count.textContent = t('exec.no_output');
  if (more) more.hidden = !view.hasMore || !!view.error;
  if (out) out.classList.toggle('is-empty', !view.count);
}

// ---------------------------------------------------------------------------
// Composer + agent question (the question takes over the input)
// ---------------------------------------------------------------------------

function questionFromToolCall(ev) {
  const args = ev.p.arguments || {};
  let options = [];
  const rawOptions = args.options || args.choices || '';
  if (rawOptions) {
    try {
      const parsed = JSON.parse(rawOptions);
      if (Array.isArray(parsed)) {
        options = parsed.slice(0, 4).map((entry, i) => (
          typeof entry === 'string'
            ? { key: String(i + 1), label: entry, detail: '' }
            : { key: String(i + 1), label: String(entry.label ?? entry.title ?? ''), detail: String(entry.detail ?? entry.description ?? '') }
        ));
      }
    } catch {
      options = [];
    }
  }
  return {
    kind: 'question',
    approvalId: '',
    capability: 'ask_user',
    who: ev.agentId || t('session.orchestrator'),
    question: String(args.question || args.summary || ''),
    detail: String(args.detail || ''),
    mandatory: false,
    options,
    runId: ev.runId,
  };
}

function askFromApproval(approval) {
  const capability = String(approval.capability || '');
  const mandatory = !!approval.mandatory_interactive || MANDATORY_CAPABILITIES.has(capability);
  return {
    kind: REVIEW_CAPABILITIES.has(capability) ? 'review' : 'approval',
    approvalId: String(approval.approval_id || ''),
    capability,
    who: shortId(approval.run_id || '') || t('session.orchestrator'),
    question: String(approval.summary || capability),
    detail: String(approval.detail || ''),
    mandatory,
    options: [],
    runId: String(approval.run_id || ''),
  };
}

// A pane never runs a mandatory-interactive operation itself (§9.3 step 5): it
// raises the question here and the shell sends the request once the user says
// yes. The permission engine still gates it — the first call mints the approval
// and the standing request goes out again after the grant.
function askFromPane(spec) {
  if (!spec || !spec.request || !spec.request.kind) return;
  state.paneRequest = spec.request;
  setAsk({
    kind: 'confirm',
    approvalId: '',
    capability: String(spec.capability || ''),
    who: String(spec.capability || ''),
    question: String(spec.summary || ''),
    detail: String(spec.detail || ''),
    mandatory: !!spec.mandatoryInteractive,
    options: [],
    runId: '',
  });
  focusAsk();
}

function setAsk(ask) {
  const changed = askSignature(ask) !== askSignature(state.ask);
  state.ask = ask;
  renderAsk();
  // The answer block lives in the console pane only. An operator reading a
  // diff, a file or the terminal when the agent raises a question would see it
  // nowhere and the run would sit blocked on an answer nobody could give.
  // Surfaced once per DISTINCT question, so repeated polling of the same
  // pending one never yanks the stage away from what is being read.
  if (ask && changed && shell && shell.dataset.stage !== 'konsola') focusAsk();
}

function clearAsk() {
  state.ask = null;
  renderAsk();
}

/// Identity of what is on screen. `loadApprovals` re-derives the ask from the
/// server on EVERY refresh, so without this the option rows are torn down and
/// rebuilt several times a second while the agent works — and a click that
/// lands between the teardown and the rebuild hits a detached node and is
/// silently lost. Answering a question must not depend on refresh timing.
function askSignature(ask) {
  if (!ask) return '';
  return [
    ask.kind, ask.approvalId, ask.capability, ask.who, ask.question, ask.detail,
    ask.mandatory ? '1' : '0', ask.runId,
    askOptions(ask).map((o) => `${o.key}:${o.value}:${o.off ? 'x' : 'o'}`).join(','),
  ].join('|');
}

let renderedAskSignature = '';

function renderAsk() {
  const answer = host.querySelector('[data-answer]');
  const composer = host.querySelector('[data-composer]');
  if (!answer || !composer) return;
  const ask = state.ask;
  composer.classList.toggle('asking', !!ask);
  if (shell) shell.dataset.ask = ask ? '1' : '0';
  // The way back to the conversation nags while the agent waits for an answer.
  paintStageStrip();
  if (!ask) {
    if (renderedAskSignature !== '') {
      answer.hidden = true;
      answer.innerHTML = '';
      renderedAskSignature = '';
    }
    paintSessionHead();
    return;
  }
  const signature = askSignature(ask);
  if (signature === renderedAskSignature && answer.childElementCount > 0) {
    // Same question, already on screen: leave the buttons alone.
    paintSessionHead();
    return;
  }
  renderedAskSignature = signature;
  const confirm = ask.kind === 'confirm';
  answer.hidden = false;
  answer.innerHTML = `
    <div class="cs-answer-head" data-action="answer-expand">
      ${sprite(confirm ? 'shield' : 'message')}
      ${escapeHtml(t(confirm ? 'ask.confirm_head' : 'ask.head'))}
      <span class="who">${escapeHtml(ask.who)}</span>
      <span class="optn">${escapeHtml(t('ask.options_count', { count: askOptions(ask).length }))}</span>
      ${sprite('chevron-down')}
    </div>
    <div class="cs-answer-q">${escapeHtml(ask.question)}${ask.detail ? ` — ${escapeHtml(ask.detail)}` : ''}</div>
    ${askExplanation(ask)}
    <div class="cs-answer-opts">${askOptions(ask).map(optionHtml).join('')}</div>
    ${ask.kind === 'question' ? '' : `
      <div class="cs-answer-foot">
        <tf-button size="sm" variant="${confirm ? 'secondary' : 'danger'}" icon="ban" data-action="answer-deny">${escapeHtml(t(confirm ? 'ask.cancel' : 'ask.deny'))}</tf-button>
      </div>`}
  `;
  paintSessionHead();
}

function askExplanation(ask) {
  const lines = [];
  if (ask.mandatory) lines.push(t('ask.mandatory'));
  if (ask.capability === DEGRADE_CAPABILITY) lines.push(t('ask.degrade'));
  if (ask.kind === 'review') lines.push(t('ask.review_gate'));
  if (!lines.length) return '';
  return `<div class="cs-answer-note">${lines.map((line) => escapeHtml(line)).join(' ')}</div>`;
}

function askOptions(ask) {
  if (ask.kind === 'question') {
    return ask.options.map((opt) => ({
      key: opt.key, label: opt.label, detail: opt.detail, action: 'answer-text', value: opt.label,
    }));
  }
  if (ask.kind === 'review') {
    return [{ key: '1', label: t('ask.review_open'), detail: t('ask.review_open_detail'), action: 'answer-review', value: '' }];
  }
  if (ask.kind === 'confirm') {
    return [{ key: '1', label: t('ask.confirm_run'), detail: t('ask.confirm_run_detail'), action: 'answer-confirm', value: '' }];
  }
  return APPROVAL_SCOPES.map((scope, i) => ({
    key: String(i + 1),
    label: t(`scope.${scope}`),
    detail: t(`scope.${scope}_detail`),
    action: 'answer-scope',
    value: scope,
    off: ask.mandatory && scope !== 'allow_once',
  }));
}

function optionHtml(opt) {
  return `
    <tf-option-row class="cs-answer-opt" marker="${escapeAttr(opt.key)}"
      label="${escapeAttr(opt.label)}" sub="${escapeAttr(opt.detail || '')}"
      ${opt.off ? 'disabled' : ''} data-action="${escapeAttr(opt.action)}"
      data-scope="${escapeAttr(opt.value || '')}"></tf-option-row>`;
}

function toggleAnswerExpanded() {
  host.querySelector('[data-answer]')?.classList.toggle('expanded');
}

function focusAsk() {
  setView('konsola');
  host.querySelector('[data-answer]')?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  host.querySelector('[data-input]')?.focus();
}

// The change list opens the NEWEST set on its own; the answer card has to land
// on the same one, otherwise the badge counts one set and the review shows
// another.
function openReviewFromAsk() {
  const open = [...state.patchSets]
    .sort((a, b) => String(b.created_at).localeCompare(String(a.created_at)))[0];
  if (!open) { toast(t('patch.none'), 'warning'); return; }
  openChangeTab(open.patch_set_id, '', '');
}

// ---------------------------------------------------------------------------
// Inspector: revision run chain, operation journal, standing grants
// ---------------------------------------------------------------------------

function renderRunChain() {
  const box = host.querySelector('[data-runs-chain]');
  if (!box) return;
  if (!state.runs.length) {
    box.innerHTML = `<div class="cs-empty"><p>${escapeHtml(t('inspector.runs_empty'))}</p></div>`;
    return;
  }
  box.innerHTML = state.runs.map((run, index) => `
    <div class="git-commit${index === 0 ? ' head' : ''}">
      <span class="gnode"><span class="gdot"></span>${index < state.runs.length - 1 ? '<span class="gline"></span>' : ''}</span>
      <span>
        <span class="gmsg">${escapeHtml(t(`run_kind.${run.kind}`))} #${escapeHtml(String(run.ordinal))} — ${escapeHtml(t(`trigger.${run.trigger}`))}</span>
        <span class="gmeta">${escapeHtml(run.agent_id || shortId(run.run_id))} · ${escapeHtml(run.status)} · ${escapeHtml(durationOf(run.started_at, run.finished_at))}</span>
        ${run.note ? `<span class="gmeta">${escapeHtml(run.note)}</span>` : ''}
      </span>
    </div>
  `).join('');
}

function renderOperations() {
  const box = host.querySelector('[data-operations]');
  if (!box) return;
  if (!state.operations.length) {
    box.innerHTML = `<div class="cs-empty"><p>${escapeHtml(t('inspector.operations_empty'))}</p></div>`;
    return;
  }
  box.innerHTML = state.operations.map((op) => {
    const unknown = op.status === 'unknown';
    const profile = profileLabel(op.mount_access, op.network_access);
    return `
      <div class="cs-insp-row">
        <span class="cs-dot ${dotClassFor(op.status === 'completed' ? 'completed' : op.status === 'failed' ? 'failed' : unknown ? 'waiting' : 'running')}"></span>
        <span class="cs-insp-txt">
          <span class="cs-insp-t">${escapeHtml(op.op_kind)} · ${escapeHtml(op.capability)}</span>
          <span class="cs-insp-s">${escapeHtml([op.origin_kind, profile, durationOf(op.started_at, op.finished_at)].filter(Boolean).join(' · '))}</span>
          ${op.error ? `<span class="cs-insp-s">${escapeHtml(op.error)}</span>` : ''}
        </span>
        <span class="m">
          <span class="cs-chip${unknown ? ' warn' : ''}">${escapeHtml(t(`op_status.${op.status}`))}</span>
          ${unknown ? `
            <tf-input size="sm" data-op-note="${escapeAttr(op.op_id)}" placeholder="${escapeAttr(t('inspector.note'))}"></tf-input>
            <tf-button size="sm" data-action="resolve-op" data-op-id="${escapeAttr(op.op_id)}" data-resolution="completed">${escapeHtml(t('inspector.resolve_completed'))}</tf-button>
            <tf-button size="sm" variant="danger" data-action="resolve-op" data-op-id="${escapeAttr(op.op_id)}" data-resolution="failed">${escapeHtml(t('inspector.resolve_failed'))}</tf-button>
          ` : ''}
        </span>
      </div>`;
  }).join('');
}

function renderGrants() {
  const box = host.querySelector('[data-grants]');
  if (!box) return;
  if (!state.grants.length) {
    box.innerHTML = `<div class="cs-empty"><p>${escapeHtml(t('inspector.grants_empty'))}</p></div>`;
    return;
  }
  box.innerHTML = state.grants.map((grant) => `
    <div class="cs-insp-row">
      <span class="cs-insp-txt">
        <span class="cs-insp-t">${escapeHtml(grant.capability)}</span>
        <span class="cs-insp-s">${escapeHtml(grant.pattern)} · ${escapeHtml(t(`grant_scope.${grant.scope}`))} · ${escapeHtml(grant.granted_by || '')}</span>
      </span>
      <span class="m">
        <tf-button size="sm" variant="danger" icon="ban" data-action="revoke-grant"
          data-capability="${escapeAttr(grant.capability)}" data-pattern="${escapeAttr(grant.pattern)}">
          ${escapeHtml(t('inspector.revoke'))}
        </tf-button>
      </span>
    </div>
  `).join('');
}

// ---------------------------------------------------------------------------
// Header projections
// ---------------------------------------------------------------------------

function paintAutonomy() {
  const chip = host.querySelector('[data-autonomy-chip]');
  if (chip) chip.innerHTML = `${sprite('shield')}${escapeHtml(t(`autonomy.${state.autonomy}`))}`;
  const select = host.querySelector('[data-autonomy]');
  if (select && select.value !== state.autonomy) select.value = state.autonomy;
}

function paintProfile() {
  const chip = host.querySelector('[data-profile-chip]');
  const text = host.querySelector('[data-profile-text]');
  if (!chip || !text) return;
  const current = state.operations.find((op) => op.status === 'pending' && op.mount_access) || null;
  const label = current ? profileLabel(current.mount_access, current.network_access) : '';
  chip.hidden = !label;
  text.textContent = label;
  state.profile = label;
}

function paintSessionHead() {
  const dot = host.querySelector('[data-session-dot]');
  const sub = host.querySelector('[data-session-sub]');
  const running = state.runs.some((run) => run.status === 'running');
  const waiting = !!state.ask;
  if (dot) dot.className = `cs-dot ${waiting ? 'ask' : running ? 'run' : 'idle'}`;
  if (sub) {
    const root = state.runs.find((run) => run.kind === 'root');
    sub.textContent = [
      t('session.orchestrator'),
      root ? durationOf(root.started_at, root.finished_at) : '',
    ].filter(Boolean).join(' · ');
  }
}

// A tab badge counts EXACTLY the rows its own navigator list holds — the agents
// badge reads the run tree off the activity widget, the changes badge is
// reported by the change list itself. Two numbers that describe one list may not
// be derived independently: the previous pair (active runs vs. files summed over
// every pending patch set) put "Zmiany 65" next to a card reading "3 pliki".
function updateCounters() {
  const runs = state.widgets.dock.runCount;
  const waiting = state.runs.some((run) => run.status === 'waiting_user');
  setCounter('agenci', runs, waiting);
  setCounter('zmiany', state.reviewCount, state.reviewCount > 0);
}

function setCounter(category, value, hot) {
  const title = t('dock.count_title', { count: value, name: t(`dock.${category}`) });
  for (const el of [
    host.querySelector(`#${DOCK_TAB_ID(category)}`),
    shell?.querySelector(`#${VIEW_TAB_ID(category)}`),
  ]) {
    if (!el) continue;
    attr(el, 'count', value ? String(value) : '');
    attr(el, 'count-tone', value && hot ? 'hot' : '');
    // The badge is a bare number, so what it counts has to be sayable somewhere.
    attr(el, 'title', value ? title : '');
  }
}

// ---------------------------------------------------------------------------
// Protocol — loading
// ---------------------------------------------------------------------------

async function bootstrap() {
  await loadRuns();
  await Promise.allSettled([
    loadApprovals(), loadOperations(), loadGrants(), loadPatchSets(), loadTasks(),
  ]);
  await loadTimeline();
}

async function loadTimeline() {
  if (state.busy) return;
  state.busy = true;
  try {
    let more = true;
    let guard = 0;
    while (more && guard < 20) {
      guard += 1;
      const resp = await ApiBinary.one('codeStudioSessionTimelineRequest', {
        workspaceId: ctx.workspaceId, sessionId: ctx.sessionId,
        afterSeq: state.cursor, limit: TIMELINE_PAGE,
      });
      const events = resp.events || [];
      ingestEvents(events);
      const next = Number(resp.next_seq ?? resp.nextSeq ?? 0);
      if (next > state.cursor) state.cursor = next;
      more = !!(resp.has_more ?? resp.hasMore) && events.length > 0;
    }
  } catch (err) {
    console.warn('[code-studio] timeline load failed:', err?.message ?? err);
  } finally {
    state.busy = false;
  }
}

async function loadRuns() {
  try {
    const resp = await ApiBinary.one('codeStudioSessionRunsRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId,
    });
    state.runs = resp.runs || [];
    for (const run of state.runs) {
      if (run.kind === 'subagent' || run.kind === 'cli') state.subagentRuns.add(run.run_id);
      // The activity widget lived on live events alone, so a page opened after
      // the turn had ended showed a run dated from the moment it was replayed
      // and no consumption at all. `session_runs` is the record: its two
      // timestamps and its token counters are what the row must state.
      const info = runInfoOf(run);
      for (const widget of [state.widgets.dock, state.widgets.now]) {
        if (widget) widget.setRunInfo(run.run_id, info);
      }
    }
    renderRunChain();
    paintSessionHead();
    paintDockEmpty();
    updateCounters();
  } catch (err) {
    console.warn('[code-studio] runs load failed:', err?.message ?? err);
  }
}

async function loadOperations() {
  try {
    const resp = await ApiBinary.one('codeStudioSessionOperationsRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, status: '', limit: 100,
    });
    state.operations = resp.operations || [];
    renderOperations();
    paintProfile();
  } catch (err) {
    console.warn('[code-studio] operations load failed:', err?.message ?? err);
  }
}

async function loadGrants() {
  try {
    const resp = await ApiBinary.one('codeStudioSessionGrantsListRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId,
    });
    state.grants = resp.grants || [];
    renderGrants();
  } catch (err) {
    console.warn('[code-studio] grants load failed:', err?.message ?? err);
  }
}

/// The session's plan. Read-only: the operator watches the same rows the build
/// loop's gate checks, and ticking one off is the working agent's job.
async function loadTasks() {
  try {
    const resp = await ApiBinary.one('codeStudioSessionTasksRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId,
    });
    state.tasks = resp.tasks || [];
    state.tasksOpen = Number(resp.open ?? 0);
    renderTasks();
  } catch (err) {
    console.warn('[code-studio] tasks load failed:', err?.message ?? err);
  }
}

const TASK_DOT = {
  done: 'ok', in_progress: 'run', blocked: 'ask', pending: 'idle',
};

function renderTasks() {
  const box = host.querySelector('[data-plan]');
  const list = host.querySelector('[data-plan-list]');
  const openEl = host.querySelector('[data-plan-open]');
  if (!box || !list) return;
  const tasks = state.tasks || [];
  box.hidden = tasks.length === 0;
  if (!tasks.length) {
    list.replaceChildren();
    return;
  }
  if (openEl) {
    openEl.textContent = state.tasksOpen > 0
      ? t('dock.plan_open', { count: state.tasksOpen })
      : t('dock.plan_done');
  }
  list.innerHTML = tasks.map((task) => {
    const status = String(task.status || 'pending');
    const note = String(task.note || '');
    return `
      <li class="cs-plan-item is-${escapeAttr(status)}">
        <span class="cs-dot ${TASK_DOT[status] || 'idle'}"></span>
        <span class="cs-plan-title">${escapeHtml(String(task.title || ''))}</span>
        ${note ? `<span class="cs-plan-note">${escapeHtml(note)}</span>` : ''}
      </li>`;
  }).join('');
}

async function loadApprovals() {
  try {
    const resp = await ApiBinary.one('codeStudioApprovalsListRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, status: 'pending',
    });
    state.approvals = resp.approvals || [];
    const pending = state.approvals.find((a) => a.status === 'pending');
    // A typed question and a pane confirmation are ours, not the server's: a
    // poll that finds no pending approval must not wipe them off the composer.
    const local = state.ask?.kind === 'question' || state.ask?.kind === 'confirm';
    // `setAsk` brings a NEW question forward on its own, so a poll that
    // re-reports the same pending approval changes nothing on screen.
    if (pending) setAsk(askFromApproval(pending));
    else if (!local) clearAsk();
    paintSessionHead();
  } catch (err) {
    console.warn('[code-studio] approvals load failed:', err?.message ?? err);
  }
}

async function loadPatchSets() {
  try {
    const resp = await ApiBinary.one('codeStudioPatchSetsListRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, status: '',
    });
    const sets = resp.patch_sets || resp.patchSets || [];
    for (const set of sets) upsertPatchSet(set);
    updateCounters();
  } catch (err) {
    console.warn('[code-studio] patch sets load failed:', err?.message ?? err);
  }
}

function pollTick() {
  if (document.hidden || !state) return;
  state.tick += 1;
  void loadTimeline();
  if (state.tick % SIDE_POLL_TICKS === 0) {
    void Promise.allSettled([loadRuns(), loadOperations(), loadApprovals()]);
  }
}

// ---------------------------------------------------------------------------
// Protocol — actions
// ---------------------------------------------------------------------------

async function sendComposer() {
  const input = host.querySelector('[data-input]');
  const message = String(input?.value || '').trim();
  if (!message) return;
  try {
    await ApiBinary.action('codeStudioSessionMessageSendRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, message,
    });
    if (input) input.value = '';
    if (state.ask?.kind === 'question') clearAsk();
    await loadTimeline();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err?.message ?? err}`, 'error');
  }
}

// A `core.ask_user` answer has no dedicated wire variant — it is a user turn
// addressed to the session's root agent, exactly like typed text.
async function answerQuestion(text) {
  const answer = String(text || '').trim();
  if (!answer) return;
  try {
    await ApiBinary.action('codeStudioSessionMessageSendRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, message: answer,
    });
    clearAsk();
    await loadTimeline();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err?.message ?? err}`, 'error');
  }
}

async function cancelSession(runId) {
  try {
    const resp = await ApiBinary.action('codeStudioSessionCancelRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, runId: runId || '',
    });
    const cancelled = (resp.cancelled_runs || resp.cancelledRuns || []).length;
    toast(t('session.cancelled', { count: cancelled }), 'info');
    await loadRuns();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err?.message ?? err}`, 'error');
  }
}

async function setAutonomy(mode) {
  if (!mode || mode === state.autonomy) return;
  try {
    const resp = await ApiBinary.action('codeStudioSessionAutonomySetRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, autonomyMode: mode,
    });
    // The server clamps to the workspace ceiling; the answer is authoritative.
    state.autonomy = String(resp.autonomy_mode || resp.autonomyMode || mode);
    paintAutonomy();
    const ceiling = String(resp.autonomy_ceiling || resp.autonomyCeiling || '');
    if (state.autonomy !== mode) {
      toast(t('autonomy.clamped', { ceiling: ceiling ? t(`autonomy.${ceiling}`) : state.autonomy }), 'warning');
    }
  } catch (err) {
    paintAutonomy();
    toast(`${I18n.t('common.error')}: ${err?.message ?? err}`, 'error');
  }
}

async function decideApproval(decision) {
  const ask = state.ask;
  if (!ask || !ask.approvalId || !decision) return;
  try {
    await ApiBinary.action('codeStudioApprovalDecideRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId,
      approvalId: ask.approvalId, decision,
    });
    clearAsk();
    if (decision === 'deny') state.paneRequest = null;
    await Promise.allSettled([loadApprovals(), loadTimeline()]);
    // The grant is what the pending pane request was waiting for.
    if (decision !== 'deny' && state.paneRequest) await sendPaneRequest();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err?.message ?? err}`, 'error');
  }
}

async function sendPaneRequest() {
  const request = state.paneRequest;
  if (!request) return;
  try {
    const body = await ApiBinary.action(request.kind, {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, ...(request.payload || {}),
    });
    // A push answers the permission question inside a successful body; a merge
    // raises it as an error. Either way the request is not done — it waits for
    // the grant and goes out again from decideApproval().
    if (String(body.status || '') === 'approval_required') {
      clearAsk();
      await loadApprovals();
      return;
    }
    state.paneRequest = null;
    clearAsk();
    applyPaneResult(request.kind, body);
    await Promise.allSettled([loadTimeline(), loadPatchSets(), loadOperations()]);
  } catch (err) {
    const message = String(err?.message ?? err);
    // The merge family raises the same question as a Conflict error.
    if (message.includes('approval_required')) {
      clearAsk();
      await loadApprovals();
      return;
    }
    state.paneRequest = null;
    clearAsk();
    toast(`${I18n.t('common.error')}: ${message}`, 'error');
  }
}

// The answer belongs to the pane that raised the question — a merge result is
// the only place the conflicting paths are ever named.
function applyPaneResult(kind, body) {
  if (kind === 'codeStudioGitMergeRequest') {
    state.panes.git?.update?.({ merge: body });
    activateTab('git', 'branch');
  } else if (kind === 'codeStudioGitMergeFinalizeRequest') {
    state.panes.git?.update?.({ finalizeStatus: String(body.status || '') });
  } else if (kind === 'codeStudioGitPushRequest') {
    const status = String(body.status || '');
    if (status === 'pushed') {
      toast(t('git.pushed', { branch: body.remote_branch || body.remoteBranch || '' }), 'success');
    } else {
      toast(body.error || t('git.push_failed'), 'error');
    }
  }
  state.docks.git?.update?.({ refresh: true });
}

async function resolveOperation(opId, resolution, button) {
  if (!opId || !resolution) return;
  const noteEl = button.closest('.cs-insp-row')?.querySelector(`[data-op-note="${CSS.escape(opId)}"]`);
  try {
    await ApiBinary.action('codeStudioOperationResolveRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId,
      opId, resolution, note: String(noteEl?.value || ''),
    });
    await loadOperations();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err?.message ?? err}`, 'error');
  }
}

async function revokeGrant(capability, pattern) {
  if (!capability) return;
  try {
    await ApiBinary.action('codeStudioSessionGrantRevokeRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId, capability, pattern: pattern || '',
    });
    await loadGrants();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err?.message ?? err}`, 'error');
  }
}
