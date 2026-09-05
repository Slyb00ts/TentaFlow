// ===== File: modules/tentaquant/project.js — one project: shell, tabs and the Pliki tab (Q06/Q07) =====
//
// The laboratory level lives in the breadcrumb, so a project draws its own,
// smaller header and its own tab bar (SPEC "Ekrany PROJEKTU"). Only the tabs
// whose screens exist are rendered: Notatnik, Studio obwodów, Runy projektu and
// Pliki. Wyniki (the pinned gallery of §13.6) arrives with `runs.tile_json` —
// a tab that answers nothing is worse than no tab.
//
// The Pliki tab is the plain CAS listing the wire offers today: upload in
// 4 MiB chunks (`FileUploadChunkRequest`), delete, and nothing else — there is
// no read-back request in the family, so no screen here pretends to open a file.

import { escapeHtml, escapeAttr, formatBytes, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import { T, sprite, fmtAgo, fmtDate, errMessage, canEditProject } from '/js/modules/tentaquant/format.js';
import { fileKindLabel, uploadFile } from '/js/modules/tentaquant/files.js';
import { drawNotebook } from '/js/modules/tentaquant/notebook.js';
import { drawStudio } from '/js/modules/tentaquant/studio.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-file-input.js';
import '/js/components/tf-table.js';
import '/js/components/tf-tabs.js';

export const PROJECT_TABS = ['notebook', 'studio', 'runs', 'files'];

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

function headerHtml(screen) {
  const project = screen.project;
  const shared = Number(project.shareCount) || 0;
  const ownership = project.myRole === 'owner'
    ? `<tf-chip status="accent" label="${escapeAttr(project.visibility === 'lab'
      ? T('project.chip_owner_lab')
      : T('project.chip_owner_private'))}"></tf-chip>`
    : `<tf-chip status="info" label="${escapeAttr(T('project.chip_role_' + project.myRole))}"></tf-chip>`;
  return `
    <div class="tf-detail-header tq-project-header">
      <div class="big-ico tq-ico">${sprite('folder')}</div>
      <div class="d-meta">
        <div class="d-name">${escapeHtml(project.name)}${ownership}</div>
        <div class="d-sub mono">${escapeHtml(project.projectId)} · ${escapeHtml(T('project.created', { when: fmtDate(project.createdAt) }))} · ${escapeHtml(T('project.updated', { when: fmtAgo(project.updatedAt) }))}</div>
        <div class="d-badges">
          <span class="tier t0">${escapeHtml(T('lab.tier_t0'))}</span>
          <tf-chip label="${escapeAttr(T('project.chip_notebooks', { n: screen.notebooks.length }))}"></tf-chip>
          <tf-chip label="${escapeAttr(T('project.chip_files', { n: screen.files.length }))}"></tf-chip>
          ${shared ? `<tf-chip label="${escapeAttr(T('project.chip_shared', { n: shared }))}"></tf-chip>` : ''}
          ${project.archivedAt ? `<tf-chip status="warn" label="${escapeAttr(T('projects.archived_chip'))}"></tf-chip>` : ''}
        </div>
      </div>
      <div class="d-actions">
        ${project.myRole === 'owner' ? `<tf-button variant="secondary" icon="share" data-act="share">${escapeHtml(T('project.action_share'))}</tf-button>` : ''}
        <tf-button variant="ghost" icon="chevron-left" data-act="back">${escapeHtml(T('project.action_back'))}</tf-button>
      </div>
    </div>`;
}

export function drawProject(screen) {
  const project = screen.project;
  if (!project) {
    screen.root.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('project.not_found'))}" message="${escapeAttr(T('project.not_found_sub'))}"></tf-alert>`;
    return;
  }
  screen.root.innerHTML = `
    <tf-breadcrumb class="tq-crumbs">
      <tf-breadcrumb-item href="#/tentaquant">${escapeHtml(T('title'))}</tf-breadcrumb-item>
      <tf-breadcrumb-item href="#/tentaquant?instance=${escapeAttr(screen.instanceId)}">${escapeHtml(screen.lab?.displayName || screen.instanceId)}</tf-breadcrumb-item>
      <tf-breadcrumb-item current>${escapeHtml(T('project.crumb', { name: project.name }))}</tf-breadcrumb-item>
    </tf-breadcrumb>
    ${headerHtml(screen)}
    <tf-tabs variant="underline" value="${escapeAttr(screen.projectTab)}" id="tq-project-tabs">
      <tf-tab id="notebook" icon="file-text">${escapeHtml(T('project.tab_notebook'))}</tf-tab>
      <tf-tab id="studio" icon="chip">${escapeHtml(T('project.tab_studio'))}</tf-tab>
      <tf-tab id="runs" icon="clock" count="${Number(project.runCount) || 0}">${escapeHtml(T('project.tab_runs'))}</tf-tab>
      <tf-tab id="files" icon="folder" count="${screen.files.length}">${escapeHtml(T('project.tab_files'))}</tf-tab>
    </tf-tabs>
    <div id="tq-project-panel"></div>`;

  screen.root.querySelector('tf-breadcrumb.tq-crumbs').addEventListener('click', (e) => {
    const link = e.target.closest('a.tf-breadcrumb-item');
    if (!link) return;
    e.preventDefault();
    // tf-breadcrumb paints its own anchors from the items, so the only thing
    // that survives onto the link is the href: the laboratory crumb is the one
    // that names an instance.
    if (link.getAttribute('href').includes('instance=')) screen.closeProject();
    else screen.backToLabs();
  });
  screen.root.querySelector('#tq-project-tabs').addEventListener('change', (e) => {
    if (e.detail.value !== screen.projectTab) screen.selectProjectTab(e.detail.value);
  });
  screen.root.querySelector('[data-act="back"]').addEventListener('click', () => screen.closeProject());
  screen.root.querySelector('[data-act="share"]')?.addEventListener('click', () => screen.openShare(project.projectId));
  drawProjectTab(screen);
}

export function drawProjectTab(screen) {
  const panel = screen.root.querySelector('#tq-project-panel');
  if (!panel) return;
  screen.disposeProjectView();
  screen.disposeRunView();
  if (screen.projectTab !== 'runs') screen.runsHost = null;
  if (screen.projectTab === 'studio') { drawStudio(screen, panel); return; }
  // The same table as the laboratory tab, narrowed by the server to this
  // project — never a client-side slice of somebody else's listing.
  if (screen.projectTab === 'runs') { screen.showRuns(panel, { projectId: screen.projectId }); return; }
  if (screen.projectTab === 'files') { drawFiles(screen, panel); return; }
  drawNotebook(screen, panel);
}

// ---------------------------------------------------------------------------
// Pliki
// ---------------------------------------------------------------------------

/// Asks, then deletes. The CAS keeps the bytes only as long as a row points at
/// them, so this is the one destructive action of the tab.
async function deleteFile(screen, file) {
  if (!file) return;
  const ok = await TfWindow.confirm({
    title: T('files.delete_title'),
    // TfWindow.confirm inserts the message as HTML, so the path is escaped here.
    message: T('files.delete_message', { name: escapeHtml(file.path) }),
    confirmLabel: T('files.delete'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await screen.tq('tentaQuantFileDeleteRequest', { projectId: screen.projectId, fileId: file.fileId });
    toast(T('files.deleted_ok', { name: file.path }), 'success');
    await screen.reloadFiles();
  } catch (err) {
    toast(`${T('files.action_failed')}: ${errMessage(err)}`, 'error');
  }
}

export function drawFiles(screen, host) {
  const editable = canEditProject(screen.project) && !screen.project.archivedAt;
  const files = screen.files.slice().sort((a, b) => String(a.path).localeCompare(String(b.path)));
  const bytes = files.reduce((sum, f) => sum + (Number(f.sizeBytes) || 0), 0);

  host.innerHTML = `
    <div class="tf-toolbar">
      <span class="tq-toolbar-title">${sprite('folder')}${escapeHtml(T('files.title'))}</span>
      <span class="tf-toolbar-spacer"></span>
      <tf-button variant="ghost" size="sm" icon="refresh" data-act="reload">${escapeHtml(T('files.reload'))}</tf-button>
    </div>
    ${editable ? `<tf-file-input id="tq-file-upload" multiple label="${escapeAttr(T('files.upload_label'))}"></tf-file-input>` : ''}
    <div class="tq-upload-status" data-upload hidden></div>
    ${files.length
      ? `<tf-table id="tq-file-table">
          <tf-column key="name" label="${escapeAttr(T('files.col_name'))}" renderer="html" fill></tf-column>
          <tf-column key="kind" label="${escapeAttr(T('files.col_kind'))}" renderer="text" nowrap></tf-column>
          <tf-column key="size" label="${escapeAttr(T('files.col_size'))}" renderer="text" nowrap></tf-column>
          <tf-column key="updated" label="${escapeAttr(T('files.col_updated'))}" renderer="text" nowrap></tf-column>
        </tf-table>`
      : `<tf-empty-state icon="folder" title="${escapeAttr(T('files.empty'))}" message="${escapeAttr(T('files.empty_sub'))}"></tf-empty-state>`}
    <div class="tq-table-footer">
      <span>${escapeHtml(T('files.footer', { n: files.length }))}</span>
      <span>${escapeHtml(T('files.footer_bytes', { size: formatBytes(bytes) }))}</span>
      <span class="tf-toolbar-spacer"></span>
      <span>${escapeHtml(T('files.hint_download'))}</span>
    </div>`;

  const table = host.querySelector('#tq-file-table');
  if (table) {
    table.rows = files.map((file) => ({
      _file: file.fileId,
      name: `<div class="tf-table__cell-title">${escapeHtml(file.path)}</div><div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(String(file.sha256 || '').slice(0, 12))}</div>`,
      kind: T('files.kind_' + fileKindLabel(file)),
      size: formatBytes(Number(file.sizeBytes) || 0),
      updated: fmtAgo(file.updatedAt),
    }));
    // Row actions go through tf-table's own hook: the cell lives in the
    // component's shadow root, where a click listener on the host would only
    // ever see the retargeted table element.
    if (editable) {
      table.rowActions = (row) => {
        const button = document.createElement('tf-button');
        button.setAttribute('variant', 'ghost');
        button.setAttribute('size', 'sm');
        button.setAttribute('icon', 'trash');
        button.setAttribute('title', T('files.delete'));
        button.addEventListener('click', () => deleteFile(screen, files.find((f) => f.fileId === row._file)));
        return button;
      };
    }
  }

  host.querySelector('[data-act="reload"]').addEventListener('click', () => screen.reloadFiles());
  host.querySelector('#tq-file-upload')?.addEventListener('change', async (e) => {
    const status = host.querySelector('[data-upload]');
    for (const file of Array.from(e.detail?.files || [])) {
      status.hidden = false;
      status.textContent = T('files.uploading', { name: file.name });
      try {
        await uploadFile(screen, file.name, new Uint8Array(await file.arrayBuffer()));
        toast(T('files.uploaded_ok', { name: file.name }), 'success');
      } catch (err) {
        toast(`${T('files.upload_failed', { name: file.name })}: ${errMessage(err)}`, 'error');
      }
    }
    status.hidden = true;
    await screen.reloadFiles();
  });
}
