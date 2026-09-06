// ===== File: modules/tentaquant/dashboard.js — Q02, the Pulpit tab of one laboratory =====
//
// The counters come from LabOverview and nothing else: projects the caller
// owns, projects shared with them by name, projects published to the whole lab,
// the week's runs split by outcome and the people the matrix admits. The two
// lists under them — recent projects and recent runs — are the rows the screen
// already loaded, so the dashboard costs no request of its own.
//
// The mockup's approvals, devices, course and IBM-account cards belong to
// features that have no backend yet; they are not drawn at all rather than
// drawn empty, and "Zacznij od" offers only the action that exists.

import { escapeHtml, escapeAttr, fmtMs } from '/js/utils.js';
import { T, sprite, fmtAgo, fmtDate, sectionOf, shortId } from '/js/modules/tentaquant/format.js';
import {
  runDurationMs, runSourceLabel, runStatusLabel, runStatusTone, runTier,
} from '/js/modules/tentaquant/run-model.js';

const RECENT_LIMIT = 5;

const SECTION_ICON = { mine: 'folder', shared: 'share', lab: 'file-text' };

// One line of context under a project's name: who it belongs to and how the
// caller reaches it. The wire already resolved the role, so this never re-derives
// access, it only names it.
function recentSubtitle(project) {
  const section = sectionOf(project);
  const parts = [];
  if (section === 'mine') {
    parts.push(T(project.visibility === 'lab' ? 'projects.visibility_lab' : 'projects.visibility_private'));
    if (Number(project.shareCount) > 0) parts.push(T('projects.shared_with', { n: Number(project.shareCount) }));
  } else {
    parts.push(T('projects.owner_named', { name: project.ownerName || project.ownerUserId }));
    parts.push(T('projects.role_' + project.myRole));
  }
  parts.push(T('dash.runs_count', { n: Number(project.runCount) || 0 }));
  return parts.join(' · ');
}

function recentRowHtml(project) {
  return `
    <div class="recent-row" data-project="${escapeAttr(project.projectId)}" role="button" tabindex="0">
      <div class="ri">${sprite(SECTION_ICON[sectionOf(project)])}</div>
      <div class="rm">
        <div class="rn">${escapeHtml(project.name)}</div>
        <div class="rs">${escapeHtml(recentSubtitle(project))}</div>
      </div>
      <span class="rt mono" title="${escapeAttr(fmtDate(project.updatedAt))}">${escapeHtml(fmtAgo(project.updatedAt))}</span>
    </div>`;
}

/// One row of "ostatnie runy": which run, out of which project, on which tier,
/// and how it ended. The same vocabulary as the Runy tab, from the same model.
function recentRunHtml(run, projectNames) {
  const tier = runTier(run);
  const duration = runDurationMs(run);
  const project = projectNames.get(run.projectId) || T('runs.no_project');
  return `
    <div class="recent-row" data-run="${escapeAttr(run.runId)}" role="button" tabindex="0">
      <div class="ri">${sprite('clock')}</div>
      <div class="rm">
        <div class="rn mono">${escapeHtml(shortId(run.runId))}</div>
        <div class="rs">${escapeHtml(`${project} · ${runSourceLabel(run)}`)}</div>
      </div>
      <span class="tier ${tier ? tier.toLowerCase() : 'off'}">${escapeHtml(tier ? T(`runs.tier_${tier.toLowerCase()}`) : run.target)}</span>
      <tf-chip status="${runStatusTone(run.status)}" label="${escapeAttr(runStatusLabel(run))}"></tf-chip>
      <span class="rt mono" title="${escapeAttr(fmtDate(run.startedAt))}">${escapeHtml(duration === null ? fmtAgo(run.startedAt) : fmtMs(duration))}</span>
    </div>`;
}

export function drawDashboard(screen, host) {
  const o = screen.overview;
  if (!o) {
    host.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('lab.load_failed'))}" message="${escapeAttr(screen.overviewError || '')}"></tf-alert>`;
    return;
  }
  const recent = screen.projects
    .filter((p) => !p.archivedAt)
    .slice()
    .sort((a, b) => String(b.updatedAt || '').localeCompare(String(a.updatedAt || '')))
    .slice(0, RECENT_LIMIT);
  const projectNames = new Map(screen.projects.map((p) => [p.projectId, p.name]));
  // `Run::List` already answers newest first, so this is a head, not a re-sort.
  const recentRuns = screen.runs.slice(0, RECENT_LIMIT);

  host.innerHTML = `
    <div class="tq-kpi">
      <tf-stat-card label="${escapeAttr(T('dash.kpi_my_projects'))}" value="${Number(o.myProjects) || 0}" icon="folder"
        delta="${escapeAttr(T('dash.kpi_my_projects_delta', { n: Number(o.sharedWithMe) || 0 }))}" delta-type="neutral"></tf-stat-card>
      <tf-stat-card label="${escapeAttr(T('dash.kpi_runs'))}" value="${Number(o.runs7dTotal) || 0}" icon="clock"
        delta="${escapeAttr(T('dash.kpi_runs_delta', {
          ok: Number(o.runs7dSucceeded) || 0,
          failed: Number(o.runs7dFailed) || 0,
          running: Number(o.runs7dRunning) || 0,
        }))}" delta-type="${Number(o.runs7dFailed) > 0 ? 'warn' : 'neutral'}"></tf-stat-card>
      <tf-stat-card label="${escapeAttr(T('dash.kpi_people'))}" value="${Number(o.peopleWithAccess) || 0}" icon="users"
        delta="${escapeAttr(T('dash.kpi_people_delta'))}" delta-type="neutral"></tf-stat-card>
      <tf-stat-card label="${escapeAttr(T('dash.kpi_lab_projects'))}" value="${Number(o.labProjects) || 0}" icon="file-text"
        delta="${escapeAttr(T('dash.kpi_changed', { when: fmtAgo(o.lastActivityAt) }))}" delta-type="neutral"></tf-stat-card>
    </div>

    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('sparkle')} ${escapeHtml(T('dash.start_title'))}</div>
        <span class="hint">${escapeHtml(T('dash.start_hint'))}</span>
      </div>
      <div class="start-grid">
        <div class="start-card" data-act="new-project" role="button" tabindex="0">
          <div class="si">${sprite('plus')}</div>
          <div class="st-title">${escapeHtml(T('dash.start_empty_title'))}</div>
          <div class="st-sub">${escapeHtml(T('dash.start_empty_sub'))}</div>
          <div class="st-cta">${escapeHtml(T('dash.start_empty_cta'))} ${sprite('arrow')}</div>
        </div>
      </div>
    </div>

    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('folder')} ${escapeHtml(T('dash.recent_title'))}</div>
        <div class="actions"><tf-button variant="ghost" size="sm" icon="chevron-right" data-act="all-projects">${escapeHtml(T('dash.recent_all'))}</tf-button></div>
      </div>
      ${recent.length
        ? recent.map(recentRowHtml).join('')
        : `<tf-empty-state icon="folder" title="${escapeAttr(T('dash.recent_empty'))}" message="${escapeAttr(T('dash.recent_empty_sub'))}"></tf-empty-state>`}
    </div>

    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('clock')} ${escapeHtml(T('dash.runs_title'))}</div>
        <div class="actions"><tf-button variant="ghost" size="sm" icon="chevron-right" data-act="all-runs">${escapeHtml(T('dash.runs_all'))}</tf-button></div>
      </div>
      ${recentRuns.length
        ? recentRuns.map((run) => recentRunHtml(run, projectNames)).join('')
        : `<tf-empty-state icon="clock" title="${escapeAttr(T('dash.runs_empty'))}" message="${escapeAttr(T('dash.runs_empty_sub'))}"></tf-empty-state>`}
    </div>`;

  host.querySelector('[data-act="new-project"]').addEventListener('click', () => screen.openNewProject());
  host.querySelector('[data-act="new-project"]').addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); screen.openNewProject(); }
  });
  host.querySelector('[data-act="all-projects"]').addEventListener('click', () => screen.selectTab('projects'));
  host.querySelector('[data-act="all-runs"]').addEventListener('click', () => screen.selectTab('runs'));
  host.querySelectorAll('.recent-row[data-run]').forEach((row) => {
    const go = () => screen.openRun(row.dataset.run);
    row.addEventListener('click', go);
    row.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go(); }
    });
  });
  host.querySelectorAll('.recent-row[data-project]').forEach((row) => {
    const go = () => screen.openProject(row.dataset.project);
    row.addEventListener('click', go);
    row.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go(); }
    });
  });
}
