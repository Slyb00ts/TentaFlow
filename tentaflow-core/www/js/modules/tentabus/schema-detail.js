// ===== File: modules/tentabus/schema-detail.js — a message pattern's page: header, the text of a version, versions, compatibility =====
//
// The page lives under the Wzory wiadomości tab (T08b): "← Wszystkie wzory",
// the pattern's name with its format, newest version and state, who added it
// and when, the topics that check with it and its compatibility; the actions
// in the header ("Wycofaj", "Usuń…", "Nowa wersja"). Beside it the text of
// one version, read-only in tf-code-editor with "Kopiuj" and "Pobierz" and
// the pattern's own description (`description` of a JSON Schema or an HL7
// profile, `doc` of an Avro record, `xs:annotation/xs:documentation` of an
// XSD), an HL7 profile spelled out as required segments and fields, the
// versions with "Wycofaj" per row, and the compatibility card with "Zmień".
// Every change goes through a window (schema-windows.js) and comes back as a
// note over the page.
//
// What topics check with is the server's rule (`registry::effective_version`):
// the newest version not withdrawn, else the newest. A withdrawn pattern
// keeps checking and says so in a warning; it takes no new version.
//
// Changes are the instance administrator's (`gate_admin`); a reader sees the
// page without the buttons and one line saying who can change it.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { patchHtml, setAttr, setText, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtDate } from '/js/modules/tentabus/format.js';
import { loadErrorHtml } from '/js/modules/tentabus/overview.js';
import { schemaFormatLabel, schemaState, compatLabel, deleteBlocker, listText } from '/js/modules/tentabus/schemas.js';
import { effectiveVersion } from '/js/modules/tentabus/schema-windows.js';
import { hl7FieldLabel, hl7SegmentLabel, orderSegments } from '/js/modules/tentabus/hl7-fields.js';
import { downloadText } from '/js/lib/download.js';
import { valueRow } from '/js/modules/tentabus/topic-settings.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-table.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

// tf-code-editor has no XML highlighter of its own; its markup one reads an XSD well.
const EDITOR_LANGUAGE = { json_schema: 'json', avro: 'json', hl7v2_profile: 'json', xsd: 'html' };
const FILE_EXTENSION = { json_schema: 'json', avro: 'avsc', protobuf: 'proto', thrift: 'thrift', xsd: 'xsd', hl7v2_profile: 'json' };
const FILE_MIME = { json_schema: 'application/schema+json', avro: 'application/json', xsd: 'application/xml', hl7v2_profile: 'application/json' };

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/** The editor language of a pattern format (plain text for one without a highlighter). */
export function editorLanguage(type) {
  return EDITOR_LANGUAGE[type] || 'plain';
}

/** The name a downloaded version gets: `wizyta-v5.json`. */
export function downloadName(subject, version, type) {
  return `${subject}-v${version}.${FILE_EXTENSION[type] || 'txt'}`;
}

const XSD_NS = 'http://www.w3.org/2001/XMLSchema';

/**
 * The first `annotation/documentation` directly under the root of an XSD, or
 * `''`. Like the server (`xsd.rs::documentation`), only elements in the XSD
 * namespace count, so both show the same description.
 */
function xsdDocumentation(text) {
  if (typeof DOMParser === 'undefined') return '';
  try {
    const doc = new DOMParser().parseFromString(String(text || ''), 'application/xml');
    const root = doc.documentElement;
    if (!root || root.localName !== 'schema' || root.namespaceURI !== XSD_NS || doc.getElementsByTagName('parsererror').length) return '';
    const inXsd = (node, name) => node.namespaceURI === XSD_NS && node.localName === name;
    for (const annotation of Array.from(root.children).filter((c) => inXsd(c, 'annotation'))) {
      for (const documentation of Array.from(annotation.children).filter((c) => inXsd(c, 'documentation'))) {
        // Only the element's own text, like the server: text of nested elements is not part of it.
        const value = Array.from(documentation.childNodes).filter((n) => n.nodeType === 3 || n.nodeType === 4).map((n) => n.nodeValue).join('').trim();
        if (value) return value;
      }
    }
  } catch {
    return '';
  }
  return '';
}

/**
 * The pattern's own description, from its text: `description` at the root of
 * a JSON Schema or an HL7 v2 profile, `doc` of an Avro record, the first
 * `xs:annotation/xs:documentation` of an XSD. `''` when it has none (or the
 * text cannot be read).
 */
export function schemaDescription(type, text) {
  if (type === 'xsd') return xsdDocumentation(text);
  if (type !== 'json_schema' && type !== 'avro' && type !== 'hl7v2_profile') return '';
  try {
    const root = JSON.parse(String(text || ''));
    const value = type === 'avro' ? root?.doc : root?.description;
    return typeof value === 'string' ? value.trim() : '';
  } catch {
    return '';
  }
}

/**
 * An HL7 v2 profile spelled out: `{ segments, fields: [{ address, label }] }`.
 * Like the server, a required field also makes its segment required, so the
 * segment list holds the listed segments and those only a field names, in the
 * order a message carries them (see `orderSegments`).
 * `label` is the dictionary name of the field ("numer pacjenta") or `''`.
 * `null` when the text is not a profile.
 */
export function hl7ProfileView(text) {
  let root;
  try {
    root = JSON.parse(String(text || ''));
  } catch {
    return null;
  }
  if (!root || typeof root !== 'object' || Array.isArray(root)) return null;
  const list = (value) => (Array.isArray(value) ? value.filter((v) => typeof v === 'string') : []);
  const fields = list(root.required_fields);
  const segments = [...list(root.required_segments)];
  for (const f of fields) {
    const segment = f.split('-')[0];
    if (segment && !segments.includes(segment)) segments.push(segment);
  }
  return { segments: orderSegments(segments), fields: fields.map((address) => ({ address, label: hl7FieldLabel(address) })) };
}

/**
 * The text as the page shows it: a pattern written in JSON is laid out with
 * indentation (patterns sent over REST often arrive on one line); "Kopiuj"
 * and "Pobierz" keep the text exactly as stored.
 */
export function displayText(type, text) {
  if (EDITOR_LANGUAGE[type] !== 'json') return text;
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

/**
 * State of one version on the page: `current` (topics check with it),
 * `older` (active, not the one checked with) or `deprecated` (withdrawn, or
 * the whole pattern is).
 */
export function versionState(version, { subjectDeprecated, effective }) {
  if (subjectDeprecated || version.deprecatedAtMs != null) return 'deprecated';
  return version.version === effective ? 'current' : 'older';
}

/** The line under the name: when and by whom it was added, and who checks with it. */
export function headerLine(info) {
  const parts = [];
  const when = info.createdAtMs ? fmtDate(info.createdAtMs) : null;
  if (when) parts.push(info.createdByLabel ? T('schemas.detail.added_by', { date: when, who: info.createdByLabel }) : T('schemas.detail.added', { date: when }));
  const topics = [...(info.usedByTopics || [])].sort();
  parts.push(topics.length ? T('schemas.detail.used_by', { topics: listText(topics), n: topics.length }) : T('schemas.detail.used_by_none'));
  return parts.join(' · ');
}

/**
 * "Wzór wycofany": what still happens with a withdrawn pattern. A reader is
 * told who can change it, not asked to do what only an administrator can.
 */
export function withdrawnText(info, effective, canAdmin) {
  const topics = [...(info.usedByTopics || [])].sort();
  if (!topics.length) return T(canAdmin ? 'schemas.detail.withdrawn_unused' : 'schemas.detail.withdrawn_unused_reader');
  return T(canAdmin ? 'schemas.detail.withdrawn_used' : 'schemas.detail.withdrawn_used_reader', { topics: listText(topics), n: topics.length, version: fmtCount(effective) });
}

/** Why a new version cannot be added, or `null` when it can. */
export function newVersionBlocker(info) {
  return info.deprecatedAtMs != null ? T('schemas.detail.no_new_version') : null;
}

/** Copies `text` to the clipboard; resolves `false` when the browser refuses. */
export async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

const backHtml = () => `<div class="tb-back"><tf-button variant="ghost" icon="chevron-left" data-go="back">${escapeHtml(T('schemas.detail.back'))}</tf-button></div>`;

function pageHtml(name) {
  return `
    <div class="tb-detail tb-schema-detail">
      ${backHtml()}
      <div class="tb-title-row">
        <div class="tb-title-main">
          <div class="tb-title-line"><h1 class="tb-title mono">${escapeHtml(name)}</h1><span class="tb-head-chips" data-role="chips"></span></div>
          <div class="tb-title-desc" data-role="desc"></div>
          <div class="tb-head-chips tb-title-badges" data-role="badges"></div>
        </div>
        <div class="tb-title-actions" data-role="actions"></div>
      </div>
      <div data-role="notice"></div>
      <div data-role="warning"></div>
      <div class="tb-schema-grid">
        <div class="section-card tb-schema-text">
          <div class="section-card-head">
            <div class="title">${sprite('file-code')} <span data-role="text-title"></span></div>
            <div class="actions">
              <tf-button variant="ghost" size="sm" icon="copy" data-go="copy" data-role="copy">${escapeHtml(T('schemas.detail.copy'))}</tf-button>
              <tf-button variant="ghost" size="sm" icon="download" data-go="download" data-role="download">${escapeHtml(T('schemas.detail.download'))}</tf-button>
            </div>
          </div>
          <div class="section-sub" data-role="about"></div>
          <div class="tb-profile" data-role="profile" hidden>
            <div class="stack">
              <div>
                <label class="tb-profile-label">${escapeHtml(T('schemas.detail.profile_segments'))}</label>
                <div class="tb-head-chips" data-role="profile-segments"></div>
                <div class="tb-vr-hint">${escapeHtml(T('schemas.detail.profile_segments_hint'))}</div>
              </div>
              <div data-role="profile-fields-wrap">
                <label class="tb-profile-label">${escapeHtml(T('schemas.detail.profile_fields'))}</label>
                <tf-table data-role="profile-fields">
                  <tf-column key="field" label="${escapeAttr(T('schemas.detail.profile_field'))}" renderer="html"></tf-column>
                  <tf-column key="contains" label="${escapeAttr(T('schemas.detail.profile_contains'))}" renderer="html" fill></tf-column>
                </tf-table>
              </div>
            </div>
          </div>
          <div class="muted" data-role="text-note"></div>
          <tf-code-editor data-role="code" readonly aria-label="${escapeAttr(T('schemas.detail.text_label'))}"></tf-code-editor>
          <div class="tb-state" data-role="text-state" hidden></div>
        </div>
        <div class="tb-schema-side">
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('layers')} ${escapeHtml(T('schemas.detail.versions'))} <tf-chip size="sm" variant="outline" status="neutral" data-role="versions-count"></tf-chip></div></div>
            <div class="section-sub" data-role="versions-sub"></div>
            <tf-table data-role="versions">
              <tf-column key="version" label="${escapeAttr(T('schemas.col_version'))}" renderer="html" fill></tf-column>
              <tf-column key="state" label="${escapeAttr(T('schemas.col_state'))}" renderer="html"></tf-column>
            </tf-table>
          </div>
          <div class="section-card" data-role="xsd-help" hidden>
            <div class="section-card-head"><div class="title">${sprite('info')} ${escapeHtml(T('schemas.detail.xsd_help_title'))}</div></div>
            <div class="section-sub">${escapeHtml(T('schemas.detail.xsd_help_works'))} ${escapeHtml(T('schemas.detail.xsd_help_fails'))}</div>
          </div>
          <div class="section-card" data-role="compat-card"></div>
        </div>
      </div>
    </div>`;
}

function missingHtml(name) {
  return `
    ${backHtml()}
    <div class="section-card">
      <tf-empty-state badge icon="file-code" title="${escapeAttr(T('schemas.detail.missing_title', { name }))}" message="${escapeAttr(T('schemas.detail.missing_text'))}">
        <tf-button variant="primary" icon="file-code" data-go="back">${escapeHtml(T('schemas.detail.back'))}</tf-button>
      </tf-empty-state>
    </div>`;
}

/**
 * Draws or repaints the page from `ctx.view()` = `{ name, info, subjectsLoaded,
 * versions, shown: { version, text, error } | null, error, errorKind,
 * canAdmin, notice, instanceLabel }` (`info` = the pattern's list row).
 * `ctx.go(action)`: `{ kind: 'back' | 'new-version' | 'withdraw' | 'delete' |
 * 'compat' | 'retry' | 'copy' | 'download' }`, `{ kind: 'withdraw-version' |
 * 'show-version', version }`.
 */
export function drawSchemaDetail(body, ctx) {
  const view = ctx.view();
  let mode = 'page';
  if (view.info ? !view.versions : !view.subjectsLoaded) mode = view.error ? `error:${view.errorKind}` : 'loading';
  else if (!view.info) mode = 'missing';
  const sig = `${view.name}|${mode}|${view.canAdmin ? 'admin' : 'reader'}`;
  if (body.__tbDetail !== sig) {
    body.__tbDetail = sig;
    if (mode === 'loading') patchHtml(body, `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`);
    else if (mode === 'missing') patchHtml(body, missingHtml(view.name));
    else if (mode.startsWith('error:')) patchHtml(body, `${backHtml()}${loadErrorHtml({ kind: view.errorKind, instanceLabel: view.instanceLabel, titleKey: 'schemas.detail.error_title' })}`);
    else {
      patchHtml(body, pageHtml(view.name));
      const table = body.querySelector('[data-role="versions"]');
      table.rowActions = versionActions(ctx);
      table.rowActionsKey = (row) => row._actionsKey;
    }
    if (!body.__tbWired) {
      body.__tbWired = true;
      body.addEventListener('click', (e) => {
        const el = e.target.closest('[data-go]');
        if (!el || !body.contains(el) || el.hasAttribute('disabled')) return;
        ctx.go({ kind: el.dataset.go });
      });
    }
  }
  if (mode === 'page') paintPage(body, view);
}

// "Pokaż" puts a version's text in the text card; "Wycofaj" (an
// administrator's, for an active version of a pattern not withdrawn) opens
// its window.
function versionActions(ctx) {
  return (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const wrap = document.createElement('div');
    wrap.className = 'tf-table__row-actions';
    if (row._canWithdraw) {
      const b = document.createElement('tf-button');
      b.setAttribute('variant', 'secondary');
      b.setAttribute('size', 'sm');
      b.dataset.act = 'withdraw-version';
      b.textContent = T('schemas.detail.withdraw_version');
      b.addEventListener('click', (e) => { e.stopPropagation(); ctx.go({ kind: 'withdraw-version', version: live()._version }); });
      wrap.appendChild(b);
    }
    if (!row._shown) {
      const b = document.createElement('tf-button');
      const label = T('schemas.detail.show_version', { version: fmtCount(row._version) });
      b.setAttribute('variant', 'ghost');
      b.setAttribute('size', 'sm');
      b.setAttribute('icon', 'eye');
      b.setAttribute('aria-label', label);
      b.title = label;
      b.textContent = T('schemas.detail.show');
      b.dataset.act = 'show-version';
      b.addEventListener('click', (e) => { e.stopPropagation(); ctx.go({ kind: 'show-version', version: live()._version }); });
      wrap.appendChild(b);
    }
    return wrap;
  };
}

/** The rows of the versions table, newest first. */
export function versionRows({ info, versions, shownVersion, canAdmin }) {
  const subjectDeprecated = info.deprecatedAtMs != null;
  const effective = effectiveVersion(subjectDeprecated, versions);
  return [...versions].sort((a, b) => b.version - a.version).map((v) => {
    const state = versionState(v, { subjectDeprecated, effective });
    const chip = {
      current: `<span class="tf-chip tf-chip--outline ok">${escapeHtml(T('schemas.detail.state_current'))}</span>`,
      older: `<span class="tf-chip tf-chip--outline">${escapeHtml(T('schemas.detail.state_older'))}</span>`,
      deprecated: `<span class="tf-chip tf-chip--outline warn">${escapeHtml(T('schemas.detail.state_deprecated'))}</span>`,
    }[state];
    const sub = [v.createdAtMs ? fmtDate(v.createdAtMs) : null, v.createdByLabel || null].filter(Boolean).join(' · ');
    const canWithdraw = canAdmin && state !== 'deprecated';
    const shown = v.version === shownVersion;
    return {
      version: `<span class="tf-table__cell-title">${escapeHtml(T('schemas.detail.version_n', { version: fmtCount(v.version) }))}</span>${sub ? `<div class="tf-table__cell-sub">${escapeHtml(sub)}</div>` : ''}`,
      state: chip,
      _key: String(v.version),
      _version: v.version,
      _canWithdraw: canWithdraw,
      _shown: shown,
      _actionsKey: `${v.version}|${canWithdraw ? 'w' : ''}|${shown ? 's' : ''}`,
    };
  });
}

function paintPage(body, view) {
  const { info, versions, canAdmin } = view;
  const subjectDeprecated = info.deprecatedAtMs != null;
  const effective = effectiveVersion(subjectDeprecated, versions);
  const state = schemaState(info);

  patchHtml(body.querySelector('[data-role="chips"]'), [
    `<tf-chip size="sm" variant="outline" status="accent" label="${escapeAttr(schemaFormatLabel(info.schemaType))}"></tf-chip>`,
    info.latestVersion != null ? `<tf-chip size="sm" variant="outline" status="neutral" label="${escapeAttr(T('schemas.detail.chip_version', { version: fmtCount(info.latestVersion) }))}"></tf-chip>` : '',
    `<tf-chip size="sm" variant="outline" status="${{ used: 'ok', unused: 'neutral', deprecated: 'warn' }[state]}" dot label="${escapeAttr(T(`schemas.state_${state}`))}" data-role="state"></tf-chip>`,
  ].join(''));
  setText(body.querySelector('[data-role="desc"]'), headerLine(info));
  patchHtml(body.querySelector('[data-role="badges"]'), `<tf-chip size="sm" variant="outline" status="info" label="${escapeAttr(T('schemas.detail.chip_compat', { compat: compatLabel(info.compatibility) }))}"></tf-chip>`);

  const blocker = deleteBlocker(info);
  const noVersion = newVersionBlocker(info);
  patchHtml(body.querySelector('[data-role="actions"]'), canAdmin
    ? `<div class="tb-title-buttons">
        <tf-button variant="secondary" icon="history" data-go="withdraw" data-role="withdraw"${subjectDeprecated ? ` disabled title="${escapeAttr(T('schemas.detail.already_withdrawn'))}"` : ''}>${escapeHtml(T('schemas.detail.withdraw'))}</tf-button>
        <tf-button variant="danger" icon="trash" data-go="delete" data-role="delete"${blocker ? ` disabled title="${escapeAttr(blocker)}"` : ''}>${escapeHtml(T('schemas.detail.delete'))}</tf-button>
        <tf-button variant="primary" icon="plus" data-go="new-version" data-role="new-version"${noVersion ? ` disabled title="${escapeAttr(noVersion)}"` : ''}>${escapeHtml(T('schemas.detail.new_version'))}</tf-button>
      </div>
      ${blocker ? `<div class="tb-title-note" data-role="delete-note">${escapeHtml(blocker)}</div>` : ''}`
    : `<div class="tb-title-note">${sprite('lock')} ${escapeHtml(T('schemas.admin_only'))}</div>`);

  const notice = view.notice;
  patchHtml(body.querySelector('[data-role="notice"]'), notice
    ? `<tf-alert tone="${escapeAttr(notice.tone || 'success')}" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}"></tf-alert>`
    : '');
  // The note of the withdrawal itself already says all the warning would.
  patchHtml(body.querySelector('[data-role="warning"]'), subjectDeprecated && !notice?.withdrawn
    ? `<tf-alert tone="warning" title="${escapeAttr(T('schemas.detail.withdrawn_title'))}" message="${escapeAttr(withdrawnText(info, effective, canAdmin))}"></tf-alert>`
    : '');

  paintText(body, view, effective);

  setAttr(body.querySelector('[data-role="versions-count"]'), 'label', fmtCount(versions.length));
  const allWithdrawn = !subjectDeprecated && versions.length > 0 && versions.every((v) => v.deprecatedAtMs != null);
  let versionsSub = T(canAdmin ? 'schemas.detail.versions_sub' : 'schemas.detail.versions_sub_reader');
  if (subjectDeprecated) versionsSub = T('schemas.detail.versions_sub_withdrawn');
  else if (allWithdrawn) versionsSub = T('schemas.detail.versions_sub_all_withdrawn', { version: fmtCount(effective) });
  setText(body.querySelector('[data-role="versions-sub"]'), versionsSub);
  body.querySelector('[data-role="xsd-help"]').hidden = info.schemaType !== 'xsd';
  setRowsIfChanged(body.querySelector('[data-role="versions"]'), versionRows({ info, versions, shownVersion: view.shown?.version ?? effective, canAdmin }));

  const compatRows = valueRow(T('schemas.detail.compat_label'), compatLabel(info.compatibility), T(`schemas.compat_desc.${info.compatibility}`));
  let compatAction = '';
  if (canAdmin && !subjectDeprecated) compatAction = `<div class="actions"><tf-button variant="secondary" size="sm" icon="edit" data-go="compat" data-role="compat">${escapeHtml(T('settings.change'))}</tf-button></div>`;
  patchHtml(body.querySelector('[data-role="compat-card"]'), `
    <div class="section-card-head"><div class="title">${sprite('shield')} ${escapeHtml(T('schemas.col_compat'))}</div>${compatAction}</div>
    <div class="section-sub">${escapeHtml(T('schemas.detail.compat_sub'))}</div>
    <div class="tb-vrows">${compatRows}</div>
    ${canAdmin && subjectDeprecated ? `<div class="tb-vr-lock">${sprite('lock')}<span>${escapeHtml(T('schemas.detail.compat_locked'))}</span></div>` : ''}`);
}

function paintText(body, view, effective) {
  const { info, shown } = view;
  const version = shown?.version ?? effective;
  setText(body.querySelector('[data-role="text-title"]'), T('schemas.detail.text_title', { version: fmtCount(version) }));
  const note = version !== effective && effective != null ? T('schemas.detail.text_not_checked', { version: fmtCount(effective) }) : '';
  const noteEl = body.querySelector('[data-role="text-note"]');
  setText(noteEl, note);
  noteEl.hidden = !note;
  const stateEl = body.querySelector('[data-role="text-state"]');
  const editor = body.querySelector('[data-role="code"]');
  const ready = shown?.text != null;
  const failed = !ready && shown?.error;
  patchHtml(stateEl, failed
    ? `${sprite('alert')}<span>${escapeHtml(T('schemas.detail.text_error', { version: fmtCount(version) }))}</span><tf-button variant="secondary" size="sm" icon="refresh" data-go="retry-text">${escapeHtml(T('shell.retry'))}</tf-button>`
    : `<tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}`);
  stateEl.hidden = ready;
  editor.hidden = !ready;
  setAttr(body.querySelector('[data-role="copy"]'), 'disabled', !ready);
  setAttr(body.querySelector('[data-role="download"]'), 'disabled', !ready);
  const about = ready ? schemaDescription(info.schemaType, shown.text) : '';
  const aboutEl = body.querySelector('[data-role="about"]');
  setText(aboutEl, about);
  aboutEl.hidden = !about;
  paintProfile(body, ready && info.schemaType === 'hl7v2_profile' ? hl7ProfileView(shown.text) : null);
  if (ready && editor.__tbText !== `${info.schemaType}\u0000${shown.text}`) {
    editor.__tbText = `${info.schemaType}\u0000${shown.text}`;
    editor.setAttribute('language', editorLanguage(info.schemaType));
    editor.value = displayText(info.schemaType, shown.text);
  }
}

function paintProfile(body, profile) {
  const el = body.querySelector('[data-role="profile"]');
  el.hidden = !profile;
  if (!profile) return;
  patchHtml(body.querySelector('[data-role="profile-segments"]'), profile.segments.length
    ? profile.segments.map((s) => {
      const name = hl7SegmentLabel(s);
      return `<tf-chip size="sm" variant="outline" status="neutral" label="${escapeAttr(name ? `${s} · ${name}` : s)}"></tf-chip>`;
    }).join('')
    : `<span class="muted">${escapeHtml(T('schemas.detail.profile_none'))}</span>`);
  body.querySelector('[data-role="profile-fields-wrap"]').hidden = !profile.fields.length;
  setRowsIfChanged(body.querySelector('[data-role="profile-fields"]'), profile.fields.map((f) => ({
    field: `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(f.address)}</span></span>`,
    contains: f.label ? escapeHtml(f.label) : '<span class="tf-table__cell-sub">—</span>',
    _key: f.address,
  })));
}

/** "Kopiuj" / "Pobierz" of the version on the page. */
export async function shareShownText(kind, view) {
  const { info, shown } = view;
  if (shown?.text == null) return;
  if (kind === 'download') {
    downloadText(downloadName(info.subject, shown.version, info.schemaType), shown.text, FILE_MIME[info.schemaType]);
    return;
  }
  const done = await copyText(shown.text);
  toast(done ? T('schemas.detail.copied', { version: fmtCount(shown.version) }) : T('schemas.detail.copy_failed'), done ? 'success' : 'error');
}
