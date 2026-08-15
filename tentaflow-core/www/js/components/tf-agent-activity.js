// =============================================================================
// File: tf-agent-activity.js — agent run activity widget (Harness plan §3.9)
// Description: <tf-agent-activity> — a compact, drill-in activity surface fed by
//   AgentRunEvent frames (§3.11 C). Three levels:
//     0 collapsed bar  : pulsing dot + current step line + background-run badge.
//                         Auto-hides when nothing runs. variant="chat-audio" is
//                         narrower (dot + badge only).
//     1 run tree        : parent → spawned/map children, per run status/agent/
//                         elapsed/tokens + cancel.
//     2 run timeline    : iterations, tool calls, compactions, router decisions,
//                         child spawns. The renderer is reused by Agents → Runs.
//   Question / permission cards surface when a run enters waiting_user.
//   Light DOM, i18n-agnostic (host passes a `labels` dict). tf-* primitives only.
//
//   Attributes: variant (chat|chat-audio), level (bar|tree|detail — pins the
//     surface to a level instead of forcing the host to synthesise a click on
//     the internal expand control; when present it also tracks internal
//     navigation, when absent the widget behaves exactly as before),
//     cards ("off" suppresses the question/permission cards for hosts that own
//     a single answering surface elsewhere — the waiting state still shows,
//     the amber dot and the waiting line stay).
// Example: const w = document.createElement('tf-agent-activity');
//          w.labels = { ... }; w.applyEvent(runEvent);
//          w.addEventListener('agent-cancel', e => cancel(e.detail.runId));
// =============================================================================

import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';

function esc(value) {
  return String(value ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

// Run status → tf-chip status tone. Mirrors the agent_runs CHECK set.
const STATUS_TONE = {
  queued: 'info',
  running: 'accent',
  waiting: 'warn',
  waiting_user: 'warn',
  completed: 'ok',
  failed: 'err',
  cancelled: 'info',
  interrupted: 'warn',
};

// child_finished status → run status (the run row updates from lifecycle events).
const TERMINAL_STATUSES = new Set(['completed', 'failed', 'cancelled', 'interrupted']);

// Default English-ish fallbacks so the component is usable without a host that
// wires labels (tests, isolated usage). A host MUST override these via `labels`.
const DEFAULT_LABELS = {
  background_one: '{n} in background',
  background_many: '{n} in background',
  iteration: 'iteration',
  idle: 'idle',
  runs_title: 'Active runs',
  no_runs: 'No active runs',
  timeline_title: 'Timeline',
  no_steps: 'No steps yet',
  cancel: 'Cancel',
  elapsed: 'elapsed',
  tokens: 'tokens',
  asks: 'asks…',
  question_send: 'Send',
  question_placeholder: 'Type an answer…',
  perm_wants: 'wants to use',
  perm_of: 'of',
  perm_deny: 'Deny',
  perm_allow_once: 'Allow once',
  perm_allow_run: 'Allow for run',
  perm_always: 'Always',
  back: 'Back',
  step_node: 'node',
  step_iteration: 'iteration',
  step_tool: 'tool',
  step_compaction: 'context compaction',
  step_router: 'router',
  step_child: 'sub-agent',
  step_question: 'question',
  step_permission: 'permission',
  step_resolved: 'resolved',
};

// A flat AgentRunEvent → a per-run step the timeline renders. Pure: shared by
// the widget AND Agents → Runs so both contexts render an identical step.
function eventToStep(ev, labels) {
  const l = { ...DEFAULT_LABELS, ...(labels || {}) };
  switch (ev.kind) {
    case 'iteration_started':
      return { tone: 'accent', kind: l.step_iteration, detail: ev.max ? `${ev.n}/${ev.max}` : String(ev.n) };
    case 'iteration_finished':
      return { tone: 'ok', kind: l.step_iteration, detail: `#${ev.n} ✓` };
    case 'tool_call_started':
      return { tone: 'accent', kind: l.step_tool, detail: ev.name };
    case 'tool_call_finished':
      return { tone: ev.status === 'ok' ? 'ok' : 'err', kind: l.step_tool, detail: `${ev.name} · ${ev.status}` };
    case 'map_element':
      return { tone: ev.status === 'ok' ? 'ok' : 'accent', kind: 'map', detail: `${ev.index + 1}/${ev.total} · ${ev.status}` };
    case 'compaction':
      return { tone: 'info', kind: l.step_compaction, detail: ev.node_id };
    case 'router_decision':
      return { tone: 'info', kind: l.step_router, detail: `${ev.selected} — ${ev.reason}` };
    case 'child_spawned':
      return { tone: 'accent', kind: l.step_child, detail: ev.agent };
    case 'child_finished':
      return { tone: TERMINAL_STATUSES.has(ev.status) && ev.status !== 'completed' ? 'warn' : 'ok', kind: l.step_child, detail: ev.status };
    case 'user_question':
      return { tone: 'warn', kind: l.step_question, detail: ev.question };
    case 'permission_request':
      return { tone: 'warn', kind: l.step_permission, detail: `${ev.addon_id}.${ev.tool_name}` };
    case 'interaction_resolved':
      return { tone: 'info', kind: l.step_resolved, detail: ev.outcome };
    case 'node_started':
      return { tone: 'accent', kind: l.step_node, detail: ev.node_type || ev.node_id };
    case 'node_finished':
      return { tone: ev.status === 'error' ? 'err' : ev.status === 'skipped' ? 'info' : 'ok', kind: l.step_node, detail: `${ev.node_id} · ${ev.status}` };
    default:
      return { tone: 'info', kind: ev.kind, detail: '' };
  }
}

// Public names for the three internal levels.
const LEVEL_NAMES = ['bar', 'tree', 'detail'];
const LEVEL_INDEX = { bar: 0, tree: 1, detail: 2 };

class TfAgentActivity extends HTMLElement {
  // `variant` stays constructor-read (property-driven, as before); only the two
  // new attributes are observed, so no existing host changes behaviour.
  static get observedAttributes() {
    return ['level', 'cards'];
  }

  constructor() {
    super();
    // runId → { runId, agent, status, parentRunId, tokens, startedAt, steps[],
    //           question?, permission?, currentStep }
    this._runs = new Map();
    this._labels = { ...DEFAULT_LABELS };
    this._level = 0; // 0 collapsed | 1 tree | 2 detail
    this._detailRunId = null;
    this._variant = this.getAttribute('variant') || 'chat';
    this._built = false;
    this._reflectingLevel = false;
    const initial = LEVEL_INDEX[(this.getAttribute('level') || '').toLowerCase()];
    if (initial !== undefined) this._level = initial;
  }

  connectedCallback() {
    if (!this._built) this._build();
    this._render();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal) return;
    if (name === 'level') {
      if (this._reflectingLevel) return;
      const n = LEVEL_INDEX[String(newVal || '').toLowerCase()];
      if (n === undefined) return;
      this._level = n;
    }
    if (this._built) this._render();
  }

  // bar | tree | detail. Setting it drives the attribute, which drives the level.
  get level() { return LEVEL_NAMES[this._level] || 'bar'; }
  set level(val) {
    const name = String(val || '').toLowerCase();
    if (!(name in LEVEL_INDEX)) return;
    this.setAttribute('level', name);
    // happy-dom and some upgrade orders skip attributeChangedCallback; the
    // assignment below is idempotent with it.
    this._level = LEVEL_INDEX[name];
    if (this._built) this._render();
  }

  // Internal navigation. The attribute is only written when the host already
  // opted into it, so an uncontrolled widget keeps its previous DOM exactly.
  _setLevel(n) {
    this._level = n;
    if (this.hasAttribute('level')) {
      this._reflectingLevel = true;
      this.setAttribute('level', LEVEL_NAMES[n]);
      this._reflectingLevel = false;
    }
    this._render();
  }

  // Hosts that own a single answering surface (a composer) set cards="off" so
  // the widget never becomes a second place to answer the same question.
  _cardsEnabled() {
    return (this.getAttribute('cards') || '').toLowerCase() !== 'off';
  }

  set labels(val) {
    this._labels = { ...DEFAULT_LABELS, ...(val || {}) };
    if (this._built) this._render();
  }

  get labels() {
    return this._labels;
  }

  set variant(val) {
    this._variant = val === 'chat-audio' ? 'chat-audio' : 'chat';
    if (this._built) this._render();
  }

  get variant() {
    return this._variant;
  }

  // Shared step renderer — reused by Agents → Runs (one renderer, two contexts).
  // `steps` is an array of { tone, kind, detail, ts? }.
  static renderTimeline(steps, labels) {
    const l = { ...DEFAULT_LABELS, ...(labels || {}) };
    if (!Array.isArray(steps) || !steps.length) {
      return `<div class="tf-aa-empty">${esc(l.no_steps)}</div>`;
    }
    const items = steps.map((s) => {
      const tone = STATUS_TONE[s.tone] ? s.tone : (s.tone || 'info');
      const ts = s.ts ? `<span class="tf-aa-step-ts">${esc(s.ts)}</span>` : '';
      const detail = s.detail ? `<span class="tf-aa-step-detail">${esc(s.detail)}</span>` : '';
      return `<li class="tf-aa-step">
        <span class="tf-aa-step-dot tone-${esc(tone)}"></span>
        <span class="tf-aa-step-kind">${esc(s.kind)}</span>
        ${detail}${ts}
      </li>`;
    }).join('');
    return `<ol class="tf-aa-timeline">${items}</ol>`;
  }

  // Builds timeline steps from a list of raw AgentRunEvent objects (used by
  // Agents → Runs when reconstructing from run_log-derived events, or live).
  static stepsFromEvents(events, labels) {
    return (Array.isArray(events) ? events : []).map((ev) => eventToStep(ev, labels));
  }

  _newRun(runId, agent) {
    return {
      runId,
      agent: agent || '',
      status: 'running',
      parentRunId: '',
      tokens: 0,
      startedAt: Date.now(),
      steps: [],
      question: null,
      permission: null,
      currentStep: '',
    };
  }

  // Apply one AgentRunEvent (decoded body). Updates the run model + re-renders.
  applyEvent(ev) {
    if (!ev || !ev.kind) return;
    const runId = ev.run_id || ev.runId || ev.scope || '';
    if (!runId) return;
    let run = this._runs.get(runId);
    if (!run) {
      run = this._newRun(runId, ev.agent || '');
      this._runs.set(runId, run);
    }

    if (ev.kind === 'child_spawned') {
      // The child row is created above by the generic path (it is keyed by
      // ev.run_id), so the parent link has to be written onto the EXISTING
      // entry — the previous "create if missing" branch never fired and every
      // spawned run stayed a root, flattening the tree.
      const childId = ev.run_id || ev.runId;
      const parentId = ev.scope || '';
      const child = childId ? this._runs.get(childId) : null;
      // Only link to a run we already hold: the tree renders from roots down, so
      // pointing at an unknown id would drop the child off the surface entirely.
      // ChildSpawned is published on the parent's own scope, so in practice the
      // parent row already exists; when it does not, the child stays a root —
      // exactly what it was before.
      if (child && parentId && parentId !== childId && !child.parentRunId
        && this._runs.has(parentId)) {
        child.parentRunId = parentId;
      }
    }

    if (ev.kind === 'child_finished') {
      const child = this._runs.get(ev.run_id || ev.runId);
      if (child) child.status = ev.status || 'completed';
    }

    if (ev.kind === 'user_question') {
      run.status = 'waiting_user';
      run.question = {
        interactionId: ev.interaction_id || ev.interactionId,
        question: ev.question || '',
        choices: Array.isArray(ev.choices) ? ev.choices : [],
      };
    } else if (ev.kind === 'permission_request') {
      run.status = 'waiting_user';
      run.permission = {
        interactionId: ev.interaction_id || ev.interactionId,
        addonId: ev.addon_id || ev.addonId || '',
        toolName: ev.tool_name || ev.toolName || '',
        permission: ev.permission || '',
      };
    } else if (ev.kind === 'interaction_resolved') {
      const iid = ev.interaction_id || ev.interactionId;
      if (run.question && run.question.interactionId === iid) run.question = null;
      if (run.permission && run.permission.interactionId === iid) run.permission = null;
      if (run.status === 'waiting_user') run.status = 'running';
    }

    const step = eventToStep(ev, this._labels);
    step.ts = new Date().toLocaleTimeString();
    run.steps.push(step);
    run.currentStep = step.detail ? `${step.kind} · ${step.detail}` : step.kind;

    if (this._built) this._render();
  }

  // Mark a run terminal (e.g. after a cancel ack) and re-render.
  setRunStatus(runId, status) {
    const run = this._runs.get(runId);
    if (run) {
      run.status = status;
      if (this._built) this._render();
    }
  }

  // How many runs the expanded tree lists. A host that puts a badge next to this
  // widget has to read the number off the widget itself — deriving it from a
  // second source is how a badge ends up disagreeing with the list under it.
  get runCount() { return this._runs.size; }

  // True when any run is still in-flight (drives auto-hide).
  hasActivity() {
    for (const run of this._runs.values()) {
      if (!TERMINAL_STATUSES.has(run.status)) return true;
    }
    return false;
  }

  // True when any run is waiting on the operator (question/grant).
  hasWaiting() {
    for (const run of this._runs.values()) {
      if (run.status === 'waiting_user') return true;
    }
    return false;
  }

  _build() {
    this.innerHTML = '';
    this._root = document.createElement('div');
    this._root.className = 'tf-agent-activity';
    this.appendChild(this._root);
    this._root.addEventListener('click', (e) => this._onClick(e));
    this._root.addEventListener('keydown', (e) => {
      const reply = e.target.closest('[data-question-input]');
      if (reply && e.key === 'Enter') {
        e.preventDefault();
        this._submitQuestion(reply.closest('[data-run]')?.dataset.run, reply.value);
      }
    });
    this._built = true;
  }

  _activeRuns() {
    return [...this._runs.values()];
  }

  _backgroundCount() {
    let n = 0;
    for (const run of this._runs.values()) {
      if (run.parentRunId && !TERMINAL_STATUSES.has(run.status)) n += 1;
    }
    return n;
  }

  _currentLine() {
    // The most recently active in-flight run drives the collapsed line.
    let best = null;
    for (const run of this._runs.values()) {
      if (TERMINAL_STATUSES.has(run.status)) continue;
      if (!best || run.startedAt >= best.startedAt) best = run;
    }
    if (!best) return this._labels.idle;
    const agent = best.agent ? `${best.agent} · ` : '';
    return `${agent}${best.currentStep || this._labels.idle}`;
  }

  _render() {
    const active = this.hasActivity();
    const waiting = this.hasWaiting();
    // Auto-hide: zero footprint when nothing runs and no card is pending.
    if (!active && !waiting && this._level === 0) {
      this._root.hidden = true;
      this._root.innerHTML = '';
      return;
    }
    this._root.hidden = false;
    this._root.dataset.variant = this._variant;
    this._root.dataset.level = String(this._level);

    if (this._level === 2 && this._detailRunId) {
      this._root.innerHTML = this._renderDetail(this._detailRunId);
      return;
    }
    if (this._level === 1) {
      this._root.innerHTML = this._renderTree();
      return;
    }
    this._root.innerHTML = this._renderBar(waiting);
  }

  _renderBar(waiting) {
    const bg = this._backgroundCount();
    const badge = bg
      ? `<span class="tf-aa-badge">${esc((bg === 1 ? this._labels.background_one : this._labels.background_many).replace('{n}', String(bg)))}</span>`
      : '';
    if (this._variant === 'chat-audio') {
      // Narrowest variant: dot + badge only.
      return `<button class="tf-aa-bar tf-aa-bar-audio ${waiting ? 'is-waiting' : ''}" data-action="expand" aria-label="agent activity">
        <span class="tf-aa-dot ${waiting ? 'is-waiting' : 'is-active'}"></span>
        ${badge}
      </button>`;
    }
    const line = waiting ? this._waitingLine() : this._currentLine();
    const waitingCard = waiting ? this._renderWaitingCards() : '';
    return `<div class="tf-aa-bar-wrap">
      <button class="tf-aa-bar ${waiting ? 'is-waiting' : ''}" data-action="expand">
        <span class="tf-aa-dot ${waiting ? 'is-waiting' : 'is-active'}"></span>
        <span class="tf-aa-line">${esc(line)}</span>
      </button>
      ${waitingCard}
    </div>`;
  }

  _waitingLine() {
    for (const run of this._runs.values()) {
      if (run.status === 'waiting_user') {
        const who = run.agent || run.runId.slice(0, 8);
        return `${who} ${this._labels.asks}`;
      }
    }
    return this._labels.idle;
  }

  _renderWaitingCards() {
    if (!this._cardsEnabled()) return '';
    const cards = [];
    for (const run of this._runs.values()) {
      if (run.question) cards.push(this._renderQuestionCard(run));
      if (run.permission) cards.push(this._renderPermissionCard(run));
    }
    return cards.join('');
  }

  _renderQuestionCard(run) {
    const q = run.question;
    const choices = (q.choices || []).slice(0, 4).map((c) =>
      `<tf-chip status="accent" clickable data-choice="${esc(c)}">${esc(c)}</tf-chip>`,
    ).join('');
    const open = !q.choices || !q.choices.length;
    const freeText = open
      ? `<div class="tf-aa-q-free">
          <input type="text" class="tf-aa-q-input" data-question-input placeholder="${esc(this._labels.question_placeholder)}" />
          <tf-button variant="primary" data-action="send-question">${esc(this._labels.question_send)}</tf-button>
        </div>`
      : '';
    return `<div class="tf-aa-card tf-aa-card-question" data-run="${esc(run.runId)}" data-interaction="${esc(q.interactionId)}">
      <div class="tf-aa-card-head">${esc(run.agent || run.runId.slice(0, 8))} ${esc(this._labels.asks)}</div>
      <div class="tf-aa-card-body">${esc(q.question)}</div>
      ${choices ? `<div class="tf-aa-choices">${choices}</div>` : ''}
      ${freeText}
    </div>`;
  }

  _renderPermissionCard(run) {
    const p = run.permission;
    return `<div class="tf-aa-card tf-aa-card-perm" data-run="${esc(run.runId)}" data-interaction="${esc(p.interactionId)}">
      <div class="tf-aa-card-head">${esc(run.agent || run.runId.slice(0, 8))}</div>
      <div class="tf-aa-card-body">${esc(this._labels.perm_wants)} <strong>${esc(p.toolName)}</strong> ${esc(this._labels.perm_of)} <strong>${esc(p.addonId)}</strong></div>
      <div class="tf-aa-perm-actions">
        <tf-button variant="ghost" data-perm="deny">${esc(this._labels.perm_deny)}</tf-button>
        <tf-button variant="ghost" data-perm="allow_once">${esc(this._labels.perm_allow_once)}</tf-button>
        <tf-button variant="ghost" data-perm="allow_for_run">${esc(this._labels.perm_allow_run)}</tf-button>
        <tf-button variant="primary" data-perm="always">${esc(this._labels.perm_always)}</tf-button>
      </div>
    </div>`;
  }

  _renderTree() {
    const runs = this._activeRuns();
    const roots = runs.filter((r) => !r.parentRunId);
    const head = `<div class="tf-aa-panel-head">
      <span class="tf-aa-panel-title">${esc(this._labels.runs_title)}</span>
      <tf-button variant="ghost" size="sm" data-action="collapse">▾</tf-button>
    </div>`;
    if (!runs.length) {
      return `${head}<div class="tf-aa-empty">${esc(this._labels.no_runs)}</div>`;
    }
    const render = (run, depth) => {
      const children = runs.filter((r) => r.parentRunId === run.runId);
      const tone = STATUS_TONE[run.status] || 'info';
      const elapsed = Math.max(0, Math.round((Date.now() - run.startedAt) / 1000));
      const cancellable = !TERMINAL_STATUSES.has(run.status);
      const row = `<div class="tf-aa-run" data-run="${esc(run.runId)}" style="--depth:${depth}">
        <button class="tf-aa-run-main" data-action="open-run" data-run-id="${esc(run.runId)}">
          <tf-chip status="${tone}" dot>${esc(run.status)}</tf-chip>
          <span class="tf-aa-run-agent">${esc(run.agent || run.runId.slice(0, 8))}</span>
          <span class="tf-aa-run-meta">${elapsed}s · ${esc(String(run.tokens))} ${esc(this._labels.tokens)}</span>
        </button>
        ${cancellable ? `<tf-button variant="ghost" size="sm" data-action="cancel-run" data-run-id="${esc(run.runId)}">${esc(this._labels.cancel)}</tf-button>` : ''}
      </div>`;
      return row + children.map((c) => render(c, depth + 1)).join('');
    };
    return `${head}<div class="tf-aa-tree">${roots.map((r) => render(r, 0)).join('')}</div>`;
  }

  _renderDetail(runId) {
    const run = this._runs.get(runId);
    if (!run) {
      this._level = 1;
      return this._renderTree();
    }
    const tone = STATUS_TONE[run.status] || 'info';
    const head = `<div class="tf-aa-panel-head">
      <tf-button variant="ghost" size="sm" data-action="to-tree">‹ ${esc(this._labels.back)}</tf-button>
      <span class="tf-aa-panel-title">${esc(run.agent || run.runId.slice(0, 8))}</span>
      <tf-chip status="${tone}" dot>${esc(run.status)}</tf-chip>
    </div>`;
    const body = TfAgentActivity.renderTimeline(run.steps, this._labels);
    return `${head}<div class="tf-aa-detail">${body}</div>`;
  }

  _onClick(e) {
    const actionEl = e.target.closest('[data-action]');
    const choiceEl = e.target.closest('[data-choice]');
    const permEl = e.target.closest('[data-perm]');

    if (choiceEl) {
      const card = choiceEl.closest('[data-run]');
      this._submitQuestion(card?.dataset.run, choiceEl.getAttribute('data-choice'));
      return;
    }
    if (permEl) {
      const card = permEl.closest('[data-run]');
      this._submitPermission(card?.dataset.run, card?.dataset.interaction, permEl.getAttribute('data-perm'));
      return;
    }
    if (!actionEl) return;
    const action = actionEl.getAttribute('data-action');
    switch (action) {
      case 'expand':
        this._setLevel(1);
        break;
      case 'collapse':
        this._setLevel(0);
        break;
      case 'open-run':
        this._detailRunId = actionEl.getAttribute('data-run-id');
        this.dispatchEvent(new CustomEvent('agent-open-run', { detail: { runId: this._detailRunId }, bubbles: true }));
        this._setLevel(2);
        break;
      case 'to-tree':
        this._setLevel(1);
        break;
      case 'cancel-run':
        this.dispatchEvent(new CustomEvent('agent-cancel', { detail: { runId: actionEl.getAttribute('data-run-id') }, bubbles: true }));
        break;
      case 'send-question': {
        const card = actionEl.closest('[data-run]');
        const input = card?.querySelector('[data-question-input]');
        this._submitQuestion(card?.dataset.run, input?.value || '');
        break;
      }
      default:
        break;
    }
  }

  _submitQuestion(runId, answer) {
    if (!runId) return;
    const run = this._runs.get(runId);
    const interactionId = run?.question?.interactionId;
    if (!interactionId || answer == null || answer === '') return;
    this.dispatchEvent(new CustomEvent('agent-reply', {
      detail: { runId, interactionId, answer: String(answer) },
      bubbles: true,
    }));
    // Optimistic dismiss — the server confirms via interaction_resolved.
    if (run) run.question = null;
    if (run && run.status === 'waiting_user') run.status = 'running';
    this._render();
  }

  _submitPermission(runId, interactionId, decision) {
    if (!runId || !interactionId || !decision) return;
    this.dispatchEvent(new CustomEvent('agent-permission', {
      detail: { runId, interactionId, decision },
      bubbles: true,
    }));
    const run = this._runs.get(runId);
    if (run) run.permission = null;
    if (run && run.status === 'waiting_user') run.status = 'running';
    this._render();
  }
}

customElements.define('tf-agent-activity', TfAgentActivity);
export { TfAgentActivity, eventToStep };
