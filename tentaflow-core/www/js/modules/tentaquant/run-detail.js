// ===== File: modules/tentaquant/run-detail.js — one run, under the Q08 table =====
//
// What a run row cannot say in seven columns: the parameters it ran with, the
// event line from the two timestamps it actually has, its outputs drawn by
// `tf-mime-output`, and the artifacts as downloads through the signed URL
// `Run::Artifact` mints (scope `TentaQuantArtifact`, one URL per click — the
// token is spent by the browser following it, so nothing here re-fetches it).
//
// A run that is still going STREAMS into this view: the same `RunSubscribe`
// session the Studio uses folds outputs and the final row in as they arrive, so
// "Otwórz run" during a live run is a live view and not a stale snapshot.
//
// Everything §13.6 asks of a RESULT — the evolution, the state views, the
// histogram with its overlays, the comparison and the scientific package — is
// the full-screen run view (`run-view.js`), which this panel opens with
// "Otwórz wynik". This one stays what it always was: the row's own facts.

import { escapeHtml, escapeAttr, formatBytes, fmtMs, toast } from '/js/utils.js';
import { T, sprite, fmtDate, errMessage, mimeLabels, shortId } from '/js/modules/tentaquant/format.js';
import { downloadUrl } from '/js/modules/tentaquant/files.js';
import {
  canControlRun, cancelRun, runDurationMs, runIsLive, runNodeName, runSourceLabel,
  runStatusLabel, runStatusTone, runTier, runTimeline, setRunPinned,
} from '/js/modules/tentaquant/run-model.js';
import {
  END_NOT_FOUND, RunStream, mergeOutputs, outputBundle,
} from '/js/modules/tentaquant/run-stream.js';
import { COUNTS_MIME, STATE_MIME } from '/js/modules/tentaquant/quantum-view.js';
import { PROBS_MIME } from '/js/components/tf-mime-output.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';

/// The recorded evolution has no renderer — it is a CBOR blob in the content
/// store — so it is the one artifact mime `tf-mime-output` deliberately does
/// not export a constant for, and it is named here, next to its download.
const KEYFRAMES_MIME = 'application/x-tentaquant-keyframes+cbor';

/// What each artifact of a run is called and saved as. A mime this build does
/// not know keeps its wire name and a `.bin` extension rather than being hidden.
const ARTIFACTS = {
  [COUNTS_MIME]: { key: 'counts', extension: 'json' },
  [STATE_MIME]: { key: 'state', extension: 'json' },
  [PROBS_MIME]: { key: 'probs', extension: 'json' },
  [KEYFRAMES_MIME]: { key: 'keyframes', extension: 'cbor' },
};

export function artifactLabel(artifact) {
  const known = ARTIFACTS[String(artifact.mime)];
  return known ? T(`runs.artifact_${known.key}`) : String(artifact.mime);
}

export function artifactFileName(run, artifact) {
  const known = ARTIFACTS[String(artifact.mime)];
  const stem = known ? known.key : 'artifact';
  return `run-${shortId(run.runId)}-${stem}.${known ? known.extension : 'bin'}`;
}

/// Where the run ran, in one line. A target this build does not know keeps its
/// wire name — the same answer `headerHtml` gives — instead of being filed
/// under a tier the row never claimed.
export function targetText(run, nodes) {
  const tier = runTier(run);
  if (!tier) return String(run.target || '');
  return `${T(`runs.tier_${tier.toLowerCase()}`)} · ${runNodeName(run, nodes)}`;
}

/// The parameter rows of the detail. Every value comes off the row or its
/// metrics — nothing here is inferred, so a field the run does not carry is
/// simply absent instead of being drawn as an em dash with a promise behind it.
export function detailRows(run, { projectName, nodes } = {}) {
  const metrics = run.metrics || {};
  const rows = [
    [T('run.field_project'), projectName || T('runs.no_project')],
    [T('run.field_source'), runSourceLabel(run)],
    [T('run.field_target'), targetText(run, nodes)],
    [T('run.field_person'), run.userName || run.userId || ''],
    [T('run.field_started'), fmtDate(run.startedAt)],
  ];
  if (run.endedAt) rows.push([T('run.field_ended'), fmtDate(run.endedAt)]);
  if (metrics.qubits) rows.push([T('run.field_register'), T('run.register_value', { q: metrics.qubits, c: metrics.clbits || 0 })]);
  if (metrics.shots) {
    rows.push([T('run.field_shots'), String(metrics.shots)]);
    // The seed is a MEASURED fact of the run (`RunMetrics`), not only an option
    // it was started with, and it is what makes the histogram above repeatable:
    // a sampled run states the number somebody would have to send back.
    rows.push([T('run.field_seed'), String(Number(metrics.seed) || 0)]);
  }
  if (metrics.method) rows.push([T('run.field_method'), `${metrics.method} · ${metrics.precision || ''}`.trim()]);
  if (metrics.backend) rows.push([T('run.field_backend'), metrics.backend]);
  if (metrics.gates) rows.push([T('run.field_gates'), T('run.gates_value', { gates: metrics.gates, keyframes: metrics.keyframes || 0 })]);
  if (metrics.memoryBytes) rows.push([T('run.field_memory'), formatBytes(Number(metrics.memoryBytes))]);
  const duration = runDurationMs(run);
  if (duration !== null) rows.push([T('run.field_duration'), fmtMs(duration)]);
  // The two "why not" fields of `RunMetrics`. They exist so a run that kept its
  // counts but stored no state, or recorded no evolution, says so in its own
  // words instead of leaving an empty section behind.
  if (metrics.stateNote) rows.push([T('run.field_state_note'), metrics.stateNote]);
  if (metrics.evolutionNote) rows.push([T('run.field_evolution_note'), metrics.evolutionNote]);
  if (run.error) rows.push([T('run.field_error'), run.error]);
  return rows;
}

/// What the stream has to say beyond the row itself. A refusal (`not_found`:
/// the run is not on this node, or is no longer readable) and a transport
/// failure both leave the status where it stood, so they are stated here
/// instead of leaving a run reading "w toku" for good; a gap says the frames
/// between were dropped, which the re-read row cannot show by itself.
export function streamNote(state, reason = '') {
  if (state && state.error) return T('run.stream_failed', { error: state.error });
  if (reason === END_NOT_FOUND) return T('run.stream_not_found');
  if (state && state.gap) return T('run.stream_gap');
  return '';
}

function kvHtml(rows) {
  return `<div class="kv">${rows
    .map(([k, v]) => `<span class="k">${escapeHtml(k)}</span><span class="v">${escapeHtml(String(v))}</span>`)
    .join('')}</div>`;
}

export function timelineHtml(run) {
  return `<div class="run-timeline">${runTimeline(run).map((item) => `
    <div class="tl-item is-${item.state}">
      <div class="tl-ico">${sprite(item.id === 'done' ? 'check' : 'clock')}</div>
      <div class="tl-body">
        <div class="tl-title">${escapeHtml(T(`run.stage_${item.id}${item.id === 'done' && item.outcome ? `_${item.outcome}` : ''}`))}</div>
        <div class="tl-time">${escapeHtml(item.at ? fmtDate(item.at) : T(`run.stage_state_${item.state}`))}</div>
      </div>
    </div>`).join('')}</div>`;
}

function headerHtml(screen, run, nodes) {
  const tier = runTier(run);
  const live = runIsLive(run);
  const mine = canControlRun(run, screen.userId);
  return `
    <div class="run-detail-head">
      <span class="rid mono">${escapeHtml(shortId(run.runId))}</span>
      <span class="tier ${tier ? tier.toLowerCase() : 'off'}">${escapeHtml(tier ? T(`runs.tier_${tier.toLowerCase()}`) : run.target)}</span>
      <span class="run-node">${escapeHtml(runNodeName(run, nodes))}</span>
      <tf-chip status="${runStatusTone(run.status)}" label="${escapeAttr(runStatusLabel(run))}"></tf-chip>
      ${run.pinnedAt ? `<tf-chip status="accent" label="${escapeAttr(T('runs.pinned'))}"></tf-chip>` : ''}
      <span class="tf-toolbar-spacer"></span>
      <tf-button variant="primary" size="sm" icon="bar-chart" data-act="result">${escapeHtml(T('run.open_result'))}</tf-button>
      ${mine ? `<tf-button variant="secondary" size="sm" icon="star" data-act="pin">${escapeHtml(T(run.pinnedAt ? 'runs.unpin' : 'runs.pin'))}</tf-button>` : ''}
      ${mine && live ? `<tf-button variant="secondary" size="sm" icon="x" data-act="cancel">${escapeHtml(T('runs.cancel'))}</tf-button>` : ''}
      <tf-button variant="ghost" size="sm" icon="x" data-act="close">${escapeHtml(T('run.close'))}</tf-button>
    </div>`;
}

function artifactsHtml(run) {
  const artifacts = run.artifacts || [];
  if (!artifacts.length) return `<div class="hint">${escapeHtml(T('run.artifacts_empty'))}</div>`;
  return `<div class="run-artifacts">${artifacts.map((artifact, index) => `
    <div class="run-artifact">
      <span class="ra-name">${escapeHtml(artifactLabel(artifact))}</span>
      <span class="ra-size mono">${escapeHtml(formatBytes(Number(artifact.sizeBytes) || 0))}</span>
      ${artifact.sha256
        ? `<tf-button variant="ghost" size="sm" icon="download" data-artifact="${index}">${escapeHtml(T('run.download'))}</tf-button>`
        : `<span class="ra-inline">${escapeHtml(T('run.inline'))}</span>`}
    </div>`).join('')}</div>`;
}

/// Renders one run under the table. The view owns a stream while the run is
/// live, so the screen is handed a disposer the way the project views are.
export function drawRunDetail(screen, host, runId, { projectId = null } = {}) {
  if (!host) return null;
  const view = new RunDetailView(screen, host, runId, projectId);
  screen.setRunViewDispose(() => view.dispose());
  // The load is the view's own business, but a caller that wants to know when
  // the panel is on screen (a test, a later focus move) has the promise.
  view.ready = view.mount();
  return view;
}

class RunDetailView {
  constructor(screen, host, runId, projectId) {
    this.screen = screen;
    this.host = host;
    this.runId = runId;
    this.projectId = projectId;
    this.run = null;
    this.stream = null;
    /// What the stream reported about ITSELF (a gap, a refusal, a failure);
    /// the row's own status is drawn from the row.
    this.note = '';
    this.ready = null;
    this.disposed = false;
    // ONE listener for the life of the view: `render` replaces the panel's
    // contents on every stream frame, and a listener added there would fire
    // once per frame that had passed. The host OUTLIVES the view — the table
    // opens every run into the same panel — so the listener is taken off it
    // again in `dispose`, or a session of clicking through runs would leave one
    // dead view (and the DOM it closed over) attached per row opened.
    this.onHostClick = (event) => this.onClick(event);
    this.host.addEventListener('click', this.onHostClick);
  }

  dispose() {
    this.disposed = true;
    this.host.removeEventListener('click', this.onHostClick);
    if (this.stream) this.stream.stop();
    this.stream = null;
  }

  async mount() {
    this.host.innerHTML = `<div class="tq-loading">${escapeHtml(T('run.loading'))}</div>`;
    try {
      const res = await this.screen.tq('tentaQuantRunGetRequest', { runId: this.runId });
      if (this.disposed) return;
      this.run = res.run;
    } catch (e) {
      this.host.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('run.load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
      return;
    }
    this.render();
    if (runIsLive(this.run)) this.follow();
  }

  /// Follows a run that has not finished. The stream carries the outputs as
  /// they are written and the final row with them, so nothing here polls.
  async follow() {
    this.stream = new RunStream(this.screen, this.runId, {
      onUpdate: (state) => this.absorb(state),
      onEnd: (reason, state) => this.finish(reason, state),
    });
    await this.stream.start();
  }

  /// The stream is over. Releasing the session drops its transport listener,
  /// and the reason it ended with becomes the note above the panel.
  finish(reason, state) {
    if (this.disposed) return;
    if (this.stream) this.stream.stop();
    this.stream = null;
    this.note = streamNote(state, reason);
    if (this.run) this.render();
  }

  absorb(state) {
    if (this.disposed || !this.run) return;
    this.note = streamNote(state);
    this.run = {
      ...this.run,
      ...(state.run || {}),
      metrics: state.metrics || (state.run && state.run.metrics) || this.run.metrics,
      artifacts: mergeOutputs(this.run.artifacts || [], state.outputs),
    };
    this.render();
  }

  render() {
    const run = this.run;
    const nodes = this.screen.lab?.nodes || [];
    const projectName = this.screen.projects.find((p) => p.projectId === run.projectId)?.name || '';
    this.host.innerHTML = `
      <div class="section-card run-detail">
        ${headerHtml(this.screen, run, nodes)}
        ${this.note ? `<tf-alert tone="warning" message="${escapeAttr(this.note)}"></tf-alert>` : ''}
        <div class="run-detail-grid">
          <div class="run-detail-side">
            <h4>${escapeHtml(T('run.params_title'))}</h4>
            ${kvHtml(detailRows(run, { projectName, nodes }))}
            <h4>${escapeHtml(T('run.timeline_title'))}</h4>
            ${timelineHtml(run)}
          </div>
          <div class="run-detail-side">
            <h4>${escapeHtml(T('run.outputs_title'))}</h4>
            <div class="run-outputs" id="tq-run-outputs"></div>
            <h4>${escapeHtml(T('run.artifacts_title'))}</h4>
            ${artifactsHtml(run)}
          </div>
        </div>
      </div>`;
    this.paintOutputs();
  }

  /// Every output that travelled inline is drawn; one that did not is a content
  /// store reference, and the artifacts list below is where it is fetched.
  paintOutputs() {
    const box = this.host.querySelector('#tq-run-outputs');
    const bundles = (this.run.artifacts || []).map(outputBundle).filter(Boolean);
    if (!bundles.length) {
      box.innerHTML = `<div class="hint">${escapeHtml(T('run.outputs_empty'))}</div>`;
      return;
    }
    box.innerHTML = '';
    for (const bundle of bundles) {
      const output = document.createElement('tf-mime-output');
      output.setAttribute('max-rows', '8');
      box.appendChild(output);
      output.labels = mimeLabels();
      output.bundle = bundle;
    }
  }

  onClick(event) {
    if (this.disposed || !this.run) return;
    const button = event.target.closest('[data-act], [data-artifact]');
    if (!button || !this.host.contains(button)) return;
    if (button.dataset.artifact !== undefined) {
      this.download(Number(button.dataset.artifact));
      return;
    }
    if (button.dataset.act === 'close') { this.screen.selectRun(null); this.host.innerHTML = ''; return; }
    // Q15 is a screen of its own, so opening it leaves this panel behind: the
    // screen disposes the stream this view holds before it draws the result.
    if (button.dataset.act === 'result') {
      this.screen.openRunResult(this.run.runId, { projectId: this.run.projectId || this.projectId });
      return;
    }
    if (button.dataset.act === 'pin') {
      setRunPinned(this.screen, this.run, !this.run.pinnedAt, { projectId: this.projectId });
      return;
    }
    if (button.dataset.act === 'cancel') cancelRun(this.screen, this.run, { projectId: this.projectId });
  }

  /// One click, one signed URL. The URL is short-lived and single-purpose, so
  /// it is minted per download instead of being cached on the row.
  async download(index) {
    const artifact = (this.run.artifacts || [])[index];
    if (!artifact || !artifact.sha256) return;
    try {
      const res = await this.screen.tq('tentaQuantRunArtifactRequest', {
        runId: this.run.runId,
        sha256: artifact.sha256,
      });
      downloadUrl(res.url, artifactFileName(this.run, artifact));
    } catch (e) {
      toast(`${T('run.download_failed')}: ${errMessage(e)}`, 'error');
    }
  }
}
