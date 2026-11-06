// ============ File: flows-builder/process-diagram.js — read-only diagram of a process run with the live token overlay ============

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { FlowCanvas } from './canvas.js';
import { processBody, processElementNames, processPlainError, processTemplates } from './bpmn.js';
import {
  ascendTo, buildTokenOverlay, canvasOverlay, createTokenFeed, drillInto, instanceIsTerminal, repeatingNodeIds, trailPath,
} from './process-token-overlay.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';

const text = (key, values) => I18n.t(`bpmn.${key}`, values);
const date = (ms) => new Date(ms).toLocaleString(I18n.getLanguage());

// A published version never changes, so its model is read once per definition and version.
const versionModels = new Map();
function readVersionModel(definitionId, version) {
  const key = `${definitionId}:${version}`;
  if (!versionModels.has(key)) {
    const pending = ApiBinary.one('processVersionGetRequest', { definitionId, version }).then((response) => response.version.model);
    pending.catch(() => versionModels.delete(key));
    versionModels.set(key, pending);
  }
  return versionModels.get(key);
}

function signatureOf(value) {
  return JSON.stringify(value, (_key, item) => (item instanceof Set ? [...item] : item instanceof Map ? [...item] : item));
}

/**
 * Mounts the diagram into `section` and returns its controller. `context`
 * supplies what lives in the instance window: `instance()` (the last snapshot),
 * `openInstance(id)`, `inspectIncident(id)`, `completeTask(summary)` and
 * `incidentText(incident)`.
 */
export function mountProcessDiagram(section, { instanceId, context }) {
  section.innerHTML = `<div class="fb-diagram-toolbar">
      <div class="fb-diagram-trail" data-trail hidden></div>
      <span class="fb-diagram-zoom">
        <tf-button variant="ghost" size="sm" icon="zoom-out" data-zoom-out aria-label="${escapeAttr(text('diagram_zoom_out'))}" title="${escapeAttr(text('diagram_zoom_out'))}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="zoom-in" data-zoom-in aria-label="${escapeAttr(text('diagram_zoom_in'))}" title="${escapeAttr(text('diagram_zoom_in'))}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="fit" data-zoom-fit aria-label="${escapeAttr(text('diagram_fit'))}" title="${escapeAttr(text('diagram_fit'))}"></tf-button>
      </span>
    </div>
    <div data-notices></div>
    <div class="fb-diagram-body">
      <div class="fb-diagram-stage fb-canvas-wrap" role="group" aria-label="${escapeAttr(text('diagram_stage'))}"><div data-canvas></div></div>
      <aside class="fb-diagram-panel" data-panel aria-live="polite"></aside>
    </div>
    <ul class="fb-diagram-legend" aria-label="${escapeAttr(text('diagram_legend'))}">
      ${['active', 'waiting', 'incident', 'visited', 'taken'].map((kind) => `<li><span class="fb-diagram-swatch" data-swatch="${kind}" aria-hidden="true"></span>${escapeHtml(text(`diagram_legend_${kind}`))}</li>`).join('')}
    </ul>`;
  const trailHost = section.querySelector('[data-trail]');
  const noticeHost = section.querySelector('[data-notices]');
  const panel = section.querySelector('[data-panel]');
  let canvas = null;
  let model = null;
  let modelKey = '';
  let shownBodyKey = '';
  let trail = [];
  let selectedNodeId = null;
  let overlay = null;
  let overlayResponse = null;
  let errorMessage = '';
  let visible = false;
  let disposed = false;
  let drawGeneration = 0;
  let focusStageAfterDraw = false;
  let renderedTrail = '';
  let renderedNotices = '';
  let renderedPanel = '';

  const feed = createTokenFeed({
    instanceId,
    request: ({ scopeId, ifRevision }) => ApiBinary.one('processInstanceTokensGetRequest', { instanceId, scopeId, ifRevision }),
    isTerminal: () => instanceIsTerminal(context.instance()?.status),
    onOverlay: (response, { fresh }) => { if (fresh || response !== overlayResponse) draw(response); },
    onError: (error) => { errorMessage = processPlainError(error.message) ?? error.message; renderNotices(); },
  });

  // Focus survives a redraw: the control that had it is found again by its key.
  function replaceKeepingFocus(host, build) {
    const focused = host.contains(document.activeElement) ? document.activeElement.closest('[data-focus-key]')?.dataset.focusKey : null;
    build();
    if (focused) host.querySelector(`[data-focus-key="${CSS.escape(focused)}"]`)?.querySelector('button, a')?.focus({ preventScroll: true });
  }

  function ensureCanvas() {
    if (canvas) return;
    canvas = new FlowCanvas(section.querySelector('[data-canvas]'), {
      mode: 'bpmn', readOnly: true,
      onSelect: (node) => { selectedNodeId = node?.id ?? null; renderPanel(); },
    });
    canvas.setTemplates(processTemplates());
  }

  async function ensureModel(definitionId, version) {
    const key = `${definitionId}:${version}`;
    if (model && key === modelKey) return;
    model = await readVersionModel(definitionId, version);
    modelKey = key;
    shownBodyKey = '';
    selectedNodeId = null;
  }

  function showBody(processId) {
    const key = `${modelKey}|${processId}|${trailPath(trail).join('/')}`;
    if (key === shownBodyKey) return;
    ensureCanvas();
    canvas.setRuntimeOverlay(null);
    canvas.setData(model);
    canvas.navigateProcessBody(trailPath(trail), processId);
    shownBodyKey = key;
    selectedNodeId = null;
  }

  async function draw(response) {
    const generation = ++drawGeneration;
    try {
      const { definitionId } = context.instance();
      await ensureModel(definitionId, response.version);
      if (disposed || generation !== drawGeneration) return;
      showBody(response.processId);
      const body = processBody(model, trailPath(trail), response.processId);
      overlay = buildTokenOverlay(response, { repeatingNodeIds: repeatingNodeIds(body) });
      overlayResponse = response;
      errorMessage = '';
      canvas.setRuntimeOverlay(canvasOverlay(overlay, text));
      if (focusStageAfterDraw) {
        focusStageAfterDraw = false;
        canvas.root.tabIndex = -1;
        canvas.root.focus({ preventScroll: true });
      }
    } catch (error) {
      if (disposed || generation !== drawGeneration) return;
      errorMessage = processPlainError(error.message) ?? error.message;
    }
    renderTrail();
    renderNotices();
    renderPanel();
  }

  function renderTrail() {
    const labels = [text('diagram_trail_root'), ...trail.map((step) => step.name)];
    const signature = signatureOf(labels);
    if (signature === renderedTrail) return;
    renderedTrail = signature;
    trailHost.hidden = trail.length === 0;
    // The breadcrumb draws its own nav inside itself, so a changed trail is a new breadcrumb.
    const crumbs = document.createElement('tf-breadcrumb');
    crumbs.append(...labels.map((label, index) => {
      const item = document.createElement('tf-breadcrumb-item');
      if (index === labels.length - 1) item.setAttribute('current', '');
      else item.setAttribute('href', '#');
      item.textContent = label;
      return item;
    }));
    trailHost.replaceChildren(crumbs);
    crumbs.querySelector('nav')?.setAttribute('aria-label', text('diagram_trail'));
  }

  function renderNotices() {
    const instance = context.instance();
    if (!instance) return;
    const notices = [];
    if (errorMessage) notices.push(['danger', text('request_error', { error: errorMessage })]);
    if (instance.variablesRedacted) notices.push(['info', text('redacted_notice')]);
    if (overlayResponse?.truncated) notices.push(['warning', text('diagram_truncated')]);
    const signature = signatureOf(notices);
    if (signature === renderedNotices) return;
    renderedNotices = signature;
    noticeHost.replaceChildren(...notices.map(([tone, message]) => {
      const alert = document.createElement('tf-alert');
      alert.setAttribute('tone', tone);
      alert.setAttribute('message', message);
      return alert;
    }));
  }

  function button(label, key, onClick, variant = 'secondary') {
    const element = document.createElement('tf-button');
    element.setAttribute('variant', variant);
    element.setAttribute('size', 'sm');
    element.dataset.focusKey = key;
    element.textContent = label;
    element.addEventListener('click', onClick);
    return element;
  }

  function occurrenceText(token) {
    const group = context.instance().repetitionGroups?.find((candidate) => candidate.groupId === token.occurrence.groupId);
    return group?.total == null ? text('repeat_ordinal', { ordinal: token.occurrence.ordinal + 1 })
      : text('repeat_of', { number: token.occurrence.ordinal + 1, total: group.total });
  }

  function drill(token, name) {
    const scope = token.wait.Scope;
    const occurrence = token.occurrence ? occurrenceText(token) : '';
    trail = drillInto(trail, { scopeId: scope.scopeId, nodeId: token.nodeId, name: occurrence ? `${name} · ${occurrence}` : name });
    selectedNodeId = null;
    focusStageAfterDraw = true;
    feed.setScope(scope.scopeId);
    renderTrail();
    renderPanel();
  }

  function tokenRow(token, names) {
    const instance = context.instance();
    const row = document.createElement('div');
    row.className = 'fb-diagram-token';
    row.dataset.tokenId = token.tokenId;
    row.dataset.waitKind = token.kind;
    const facts = [text('diagram_since', { time: date(token.sinceMs) })];
    if (token.occurrence) facts.unshift(occurrenceText(token));
    if (token.kind === 'Timer' && token.wait.Timer.dueAtMs != null) facts.push(text('diagram_due', { time: date(token.wait.Timer.dueAtMs) }));
    if (token.kind === 'Scope') facts.push(text('diagram_inside', { count: token.wait.Scope.activeTokens }));
    row.innerHTML = `<strong>${escapeHtml(text(`diagram_wait_${token.kind.toLowerCase()}`))}</strong>${facts.map((fact) => `<p>${escapeHtml(fact)}</p>`).join('')}`;
    const actions = document.createElement('div');
    actions.className = 'fb-diagram-actions';
    for (const incidentId of token.incidentIds ?? []) {
      const incident = instance.incidents?.find((candidate) => candidate.incidentId === incidentId);
      const note = document.createElement('p');
      note.className = 'fb-diagram-incident';
      note.textContent = incident ? context.incidentText(incident) : text('diagram_incident_unloaded');
      row.append(note);
      if (incident) actions.append(button(text('inspect_incident'), `incident:${incidentId}`, () => context.inspectIncident(incidentId)));
    }
    if (token.kind === 'UserTask') {
      const task = [instance.selectedUserTask, ...instance.userTasks].find((candidate) => candidate?.userTaskId === token.wait.UserTask.userTaskId);
      if (task?.canComplete && task.status === 'Open') {
        const label = text(task.kind === 'Verification' ? 'review_result' : task.kind === 'Manual' ? 'acknowledge_manual' : 'complete_work');
        actions.append(button(label, `task:${task.userTaskId}`, () => context.completeTask(task), 'primary'));
      }
    }
    if (token.kind === 'Call') {
      const childId = token.wait.Call.childInstanceId;
      const related = instance.calls?.find((call) => call.Outgoing?.child?.instanceId === childId)?.Outgoing.child;
      if (related?.canOpen) actions.append(button(text('call_open_child'), `call:${childId}`, () => context.openInstance(childId)));
    }
    if (token.kind === 'Scope') {
      const name = names.get(token.nodeId) ?? text('element_unavailable');
      actions.append(button(text('diagram_open_scope'), `scope:${token.wait.Scope.scopeId}`, () => drill(token, name)));
    }
    if (actions.childElementCount) row.append(actions);
    return row;
  }

  function renderPanel() {
    const instance = context.instance();
    if (!instance) return;
    const names = model ? processElementNames(model) : new Map();
    const entry = selectedNodeId ? overlay?.nodes.get(selectedNodeId) : null;
    const signature = signatureOf([selectedNodeId, entry, overlay?.visitedNodeIds.has(selectedNodeId), instance.revision, instance.variablesRedacted, I18n.getLanguage()]);
    if (signature === renderedPanel) return;
    renderedPanel = signature;
    replaceKeepingFocus(panel, () => {
      panel.replaceChildren();
      if (!selectedNodeId) {
        const hint = document.createElement('p');
        hint.className = 'fb-diagram-hint';
        hint.textContent = overlay ? text('diagram_panel_hint') : text('diagram_loading');
        panel.append(hint);
        return;
      }
      const heading = document.createElement('h4');
      heading.textContent = names.get(selectedNodeId) ?? text('element_unavailable');
      panel.append(heading);
      if (!entry || entry.count === 0 && entry.incidentIds.length === 0) {
        const none = document.createElement('p');
        none.textContent = overlay.visitedNodeIds.has(selectedNodeId) ? text('diagram_visited_here') : text('diagram_no_tokens');
        panel.append(none);
        return;
      }
      if (entry.occurrenceCount > 0) {
        const group = instance.repetitionGroups?.find((candidate) => candidate.nodeId === selectedNodeId);
        const progress = document.createElement('p');
        progress.textContent = group?.total == null ? text('diagram_repeat_active_unknown', { active: entry.occurrenceCount })
          : text('diagram_repeat_active', { active: entry.occurrenceCount, total: group.total });
        panel.append(progress);
      }
      for (const token of entry.tokens) panel.append(tokenRow(token, names));
      if (entry.tokens.length === 0) {
        for (const incidentId of entry.incidentIds) {
          const incident = instance.incidents?.find((candidate) => candidate.incidentId === incidentId);
          const note = document.createElement('p');
          note.className = 'fb-diagram-incident';
          note.textContent = incident ? context.incidentText(incident) : text('diagram_incident_unloaded');
          panel.append(note);
          if (incident) panel.append(button(text('inspect_incident'), `incident:${incidentId}`, () => context.inspectIncident(incidentId)));
        }
      }
    });
  }

  trailHost.addEventListener('click', (event) => {
    const link = event.target.closest('a.tf-breadcrumb-item');
    if (!link) return;
    event.preventDefault();
    const depth = [...trailHost.querySelectorAll('a.tf-breadcrumb-item')].indexOf(link);
    trail = ascendTo(trail, depth);
    selectedNodeId = null;
    focusStageAfterDraw = true;
    feed.setScope(trail.at(-1)?.scopeId ?? null);
    renderTrail();
    renderPanel();
  });
  section.querySelector('[data-zoom-in]').addEventListener('click', () => canvas?.zoomBy(1.25));
  section.querySelector('[data-zoom-out]').addEventListener('click', () => canvas?.zoomBy(1 / 1.25));
  section.querySelector('[data-zoom-fit]').addEventListener('click', () => canvas?.fitToContent());

  // A hidden page or diagram reads nothing; coming back reads at once.
  function sync() {
    if (disposed) return;
    if (visible && !document.hidden) feed.start();
    else feed.stop();
  }
  document.addEventListener('visibilitychange', sync);
  renderTrail();

  return {
    show() { visible = true; section.hidden = false; ensureCanvas(); renderPanel(); sync(); },
    hide() { visible = false; section.hidden = true; sync(); },
    /** The instance snapshot changed: read the tokens soon and refresh what depends on it. */
    changed() {
      if (disposed || !visible) return;
      renderNotices();
      renderPanel();
      feed.poke();
    },
    get canvas() { return canvas; },
    dispose() {
      disposed = true;
      document.removeEventListener('visibilitychange', sync);
      feed.dispose();
      canvas?.destroy();
      canvas = null;
    },
  };
}
