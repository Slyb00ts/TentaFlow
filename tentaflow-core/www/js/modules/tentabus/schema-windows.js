// ===== File: modules/tentabus/schema-windows.js — the windows over message patterns: add, new version, compatibility, withdraw, delete =====
//
// What the server does, which each window states before anyone confirms
// (tentaflow-core/src/bus/schema_registry/registry.rs):
//   - a new pattern starts at version 1 with the compatibility chosen here; a
//     name that already exists would silently become a new VERSION of that
//     pattern, so the add window refuses a listed name;
//   - a new version is compared with the newest one under the pattern's
//     compatibility and refused when it breaks it; the same text as an
//     existing version adds nothing (the server answers that version);
//   - topics check with the newest version that is not withdrawn; when every
//     version is withdrawn, or the whole pattern is, they keep checking with
//     the newest one — withdrawing never switches checking off;
//   - a withdrawn pattern takes no new version and cannot be chosen for a
//     topic; its versions can still be read and downloaded;
//   - a pattern a topic uses cannot be deleted.
// A refused new version is explained in plain words by the reason the server
// gives (`SchemaIncompatible.detail`, a fixed set of sentences of the JSON
// Schema checker); a reason this screen does not know is shown as the
// server wrote it, under a line that still says what happened.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { T, fmtCount } from '/js/modules/tentabus/format.js';
import { COMPATIBILITIES, compatLabel, schemaFormatLabel, schemaKind, listText } from '/js/modules/tentabus/schemas.js';
import { openChangeWindow, openConfirmWindow } from '/js/modules/tentabus/windows.js';
import { openRetypeDialog } from '/js/lib/retype-dialog.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-file-input.js';
import '/js/components/tf-choice-card.js';
import '/js/components/tf-radio.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/** `registry::MAX_SUBJECT_NAME_LEN`. */
export const NAME_MAX = 128;
/** `schema_registry::MAX_SCHEMA_TEXT_BYTES`. */
export const TEXT_MAX_BYTES = 256 * 1024;
/** What a new pattern checks unless the adding person picks otherwise (the server's own default). */
export const DEFAULT_COMPATIBILITY = 'backward';
/** Formats whose text is a JSON document, checked here before it is sent. */
const JSON_TEXT_TYPES = new Set(['json_schema', 'avro', 'hl7v2_profile']);

const byteLength = (text) => new TextEncoder().encode(String(text)).length;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/**
 * Why a new pattern's name cannot be used, or `null` when it can: `empty`,
 * `invalid` (characters outside `[A-Za-z0-9._-]` or longer than 128 bytes) or
 * `taken` (a pattern of that name exists — the server would add a version to it).
 */
export function subjectNameProblem(name, existingNames = []) {
  const text = String(name || '').trim();
  if (!text) return 'empty';
  if (byteLength(text) > NAME_MAX || !/^[A-Za-z0-9._-]+$/.test(text)) return 'invalid';
  if (existingNames.includes(text)) return 'taken';
  return null;
}

/** Whether `text` is a well-formed XML document; assumed so where there is no XML parser to ask. */
function wellFormedXml(text) {
  if (typeof DOMParser === 'undefined') return true;
  try {
    return new DOMParser().parseFromString(String(text), 'application/xml').getElementsByTagName('parsererror').length === 0;
  } catch {
    return false;
  }
}

/**
 * Why a pattern text cannot be sent, or `null` when it can: `empty`,
 * `too_big` (over 256 KB), `not_json` for a format written in JSON or
 * `not_xml` for an XSD.
 */
export function schemaTextProblem(text, schemaType) {
  const value = String(text || '');
  if (!value.trim()) return 'empty';
  if (byteLength(value) > TEXT_MAX_BYTES) return 'too_big';
  if (schemaType === 'xsd' && !wellFormedXml(value)) return 'not_xml';
  if (JSON_TEXT_TYPES.has(schemaType)) {
    try {
      JSON.parse(value);
    } catch {
      return 'not_json';
    }
  }
  return null;
}

/** `SchemaRegisterRequest` of a new pattern (`compatibility` set) or of a new version (left to the pattern's). */
export function buildRegisterRequest(instanceId, { subject, schemaType, schemaText, compatibility = null }) {
  const request = { instanceId, subject, schemaType, schemaText };
  if (compatibility) request.compatibility = compatibility;
  return request;
}

/** `SchemaDeleteRequest`: withdraw the pattern, withdraw one version, or delete the pattern. */
export function buildDeleteRequest(instanceId, subject, { version = null, deprecateOnly }) {
  const request = { instanceId, subject, deprecateOnly: Boolean(deprecateOnly) };
  if (version != null) request.version = Number(version);
  return request;
}

function parseObject(text) {
  try {
    const value = JSON.parse(String(text || ''));
    return value && typeof value === 'object' && !Array.isArray(value) ? value : null;
  } catch {
    return null;
  }
}

const topFields = (schema) => Object.keys(schema?.properties && typeof schema.properties === 'object' ? schema.properties : {});
const requiredFields = (schema) => (Array.isArray(schema?.required) ? schema.required.filter((f) => typeof f === 'string') : []);

function deepEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== typeof b || a === null || b === null || typeof a !== 'object') return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  return ka.length === kb.length && ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && deepEqual(a[k], b[k]));
}

/**
 * How a new JSON Schema text differs from the newest version, in words — the
 * top-level fields added, removed and made (not) required. `null` when either
 * text is not a JSON object (another format, or text still being written).
 */
export function jsonSchemaChanges(oldText, newText, version) {
  const before = parseObject(oldText);
  const after = parseObject(newText);
  if (!before || !after) return null;
  const v = fmtCount(version);
  if (deepEqual(before, after)) return T('schemas.version.same_as', { version: v });
  const oldFields = new Set(topFields(before));
  const newFields = new Set(topFields(after));
  const oldReq = new Set(requiredFields(before));
  const newReq = new Set(requiredFields(after));
  const parts = [];
  for (const f of newFields) {
    if (!oldFields.has(f)) parts.push(T(newReq.has(f) ? 'schemas.version.diff_added_required' : 'schemas.version.diff_added_optional', { field: f }));
  }
  for (const f of oldFields) if (!newFields.has(f)) parts.push(T('schemas.version.diff_removed', { field: f }));
  for (const f of newFields) {
    if (!oldFields.has(f)) continue;
    if (newReq.has(f) && !oldReq.has(f)) parts.push(T('schemas.version.diff_now_required', { field: f }));
    if (!newReq.has(f) && oldReq.has(f)) parts.push(T('schemas.version.diff_now_optional', { field: f }));
  }
  if (!parts.length) return T('schemas.version.diff_other', { version: v });
  return T('schemas.version.diff', { version: v, changes: listText(parts) });
}

/**
 * An HL7 v2 profile as sets, the way the server compares them: a required
 * field also makes its segment required. `null` when the text is not a profile object.
 */
function profileSets(text) {
  const profile = parseObject(text);
  if (!profile) return null;
  const list = (value) => (Array.isArray(value) ? value.filter((v) => typeof v === 'string') : []);
  const fields = new Set(list(profile.required_fields));
  const segments = new Set(list(profile.required_segments));
  for (const f of fields) segments.add(f.split('-')[0]);
  return { segments, fields };
}

/**
 * How a new HL7 v2 profile text differs from the newest version, in words —
 * the required segments and fields added and removed (a required field makes
 * its segment required, so both are named). `null` when either text is not a
 * profile object.
 */
export function profileChanges(oldText, newText, version) {
  const before = profileSets(oldText);
  const after = profileSets(newText);
  if (!before || !after) return null;
  const v = fmtCount(version);
  const gained = (a, b) => [...a].filter((x) => !b.has(x));
  const addedFields = gained(after.fields, before.fields);
  const removedFields = gained(before.fields, after.fields);
  const addedSegments = gained(after.segments, before.segments);
  const removedSegments = gained(before.segments, after.segments);
  const parts = [
    ...addedSegments.map((segment) => T('schemas.version.diff_segment_added', { segment })),
    ...removedSegments.map((segment) => T('schemas.version.diff_segment_removed', { segment })),
    ...addedFields.map((field) => T('schemas.version.diff_added_required', { field })),
    ...removedFields.map((field) => T('schemas.version.diff_now_optional', { field })),
  ];
  if (!parts.length) {
    return deepEqual(parseObject(oldText), parseObject(newText))
      ? T('schemas.version.same_as', { version: v })
      : T('schemas.version.diff_other', { version: v });
  }
  return T('schemas.version.diff', { version: v, changes: listText(parts) });
}

// The sentences of the HL7 v2 profile checker (hl7v2_profile.rs): which side
// asks for more, then `segments [A, B] are not guaranteed; fields [X] are not guaranteed`.
const HL7_BACKWARD = /^backward \(the new profile requires more than the old one guarantees\): ([\s\S]*)$/;
const HL7_FORWARD = /^forward \(the old profile requires more than the new one guarantees\): ([\s\S]*)$/;

function hl7Items(rest) {
  const grab = (kind) => {
    const m = new RegExp(`${kind} \\[([^\\]]*)\\] are not guaranteed`).exec(rest);
    return m ? m[1].split(',').map((x) => x.trim()).filter(Boolean) : [];
  };
  const segments = grab('segments');
  const fields = grab('fields');
  return { segments, fields, items: [...fields, ...segments.filter((s) => !fields.some((f) => f.startsWith(`${s}-`)))] };
}

/**
 * What the new version of an HL7 v2 profile asks for beyond the old one — the
 * server's refusal under "nowe programy przeczytają stare wiadomości" —
 * `{ segments, fields, items }`, or `null` for any other refusal. Dropping
 * these from the required lists is the fix `dropRequired` applies.
 */
export function hl7DropOffer(detail) {
  const m = HL7_BACKWARD.exec(String(detail || ''));
  if (!m) return null;
  const offer = hl7Items(m[1]);
  return offer.items.length ? offer : null;
}

/** The profile text without the given required segments and fields, laid out for editing; `null` when it is not a JSON object. */
export function dropRequired(text, { segments = [], fields = [] }) {
  const profile = parseObject(text);
  if (!profile) return null;
  const without = (list, drop) => (Array.isArray(list) ? list.filter((x) => !drop.includes(x)) : list);
  const next = { ...profile };
  if ('required_segments' in profile) next.required_segments = without(profile.required_segments, segments);
  if ('required_fields' in profile) next.required_fields = without(profile.required_fields, fields);
  return JSON.stringify(next, null, 2);
}

/**
 * The server's refusal of a new version, taken apart: `{ mode, detail }` from
 * `bus.schema_incompatible: '<subject>' mode=<mode>: <detail>`, `null` for any
 * other error.
 */
export function parseIncompatible(message) {
  const m = /\bbus\.schema_incompatible: '[^']*' mode=([a-z]+): ([\s\S]*)$/.exec(String(message || ''));
  return m ? { mode: m[1], detail: m[2].trim() } : null;
}

/** The types a field of a JSON Schema allows, or `null` for no `type` (anything). */
function fieldTypes(schema, field) {
  const type = schema?.properties?.[field]?.type;
  if (type == null) return null;
  return (Array.isArray(type) ? type : [type]).map(String).sort();
}

/**
 * The reader's types in the checker's sentence `… reader allows Some({"a", "b"})`
 * (`None` = anything), or `undefined` when the sentence does not say.
 */
function readerTypes(detail) {
  const m = /reader allows (?:Some\(\{([^}]*)\}\)|(None))/.exec(detail);
  if (!m) return undefined;
  if (m[2]) return null;
  return [...m[1].matchAll(/"([^"]*)"/g)].map((x) => x[1]).sort();
}

const sameTypes = (a, b) => a !== undefined && JSON.stringify(a) === JSON.stringify(b);

/**
 * The reason a new version breaks the compatibility, in words, with what to
 * do about it — `{ reason, fix }` — or `null` for a reason this screen does
 * not know. `newText` tells the direction where the server's sentence does
 * not: under "w obie strony" both directions are checked with the same words.
 */
export function incompatibilityReason({ mode, detail, newText }) {
  const after = parseObject(newText);
  const quoted = (re) => re.exec(detail)?.[1] ?? null;
  const generic = T('schemas.incompat.fix_generic');
  // An HL7 v2 profile: the sentence itself says which side asks for more.
  const profileNew = HL7_BACKWARD.exec(detail);
  const profileOld = HL7_FORWARD.exec(detail);
  if (profileNew || profileOld) {
    const field = hl7Items((profileNew || profileOld)[1]).items.join(', ');
    return profileNew
      ? { reason: T('schemas.incompat.required_new', { field }), fix: T('schemas.incompat.fix_required_new', { field }) }
      : { reason: T('schemas.incompat.required_old', { field }), fix: T('schemas.incompat.fix_required_old', { field }) };
  }
  let field = quoted(/^property '([^']+)' is required by the reader schema but not guaranteed present by the writer schema/);
  if (field != null) {
    // The reader is the new version under "backward", the old one under "forward".
    const newRequires = mode === 'backward' || (mode === 'full' && requiredFields(after).includes(field));
    return newRequires
      ? { reason: T('schemas.incompat.required_new', { field }), fix: T('schemas.incompat.fix_required_new', { field }) }
      : { reason: T('schemas.incompat.required_old', { field }), fix: T('schemas.incompat.fix_required_old', { field }) };
  }
  field = quoted(/^property '([^']+)' present in the writer schema has no counterpart in the reader schema/);
  if (field != null) {
    const added = topFields(after).includes(field);
    return { reason: T(added ? 'schemas.incompat.extra_added' : 'schemas.incompat.extra_unknown', { field }), fix: generic };
  }
  field = quoted(/^property '([^']+)' type is narrowed/);
  if (field != null) {
    // The reader is the new version under "backward", the old one under
    // "forward"; under "w obie strony" the reader's types in the sentence
    // tell which side it was.
    const newIsReader = mode === 'backward' || (mode === 'full' && sameTypes(readerTypes(detail), fieldTypes(after, field)));
    return { reason: T(newIsReader ? 'schemas.incompat.narrowed' : 'schemas.incompat.widened', { field }), fix: generic };
  }
  field = quoted(/^property '([^']+)' subschema differs beyond 'type' widening/);
  if (field != null) return { reason: T('schemas.incompat.changed', { field }), fix: generic };
  if (/^writer schema allows additional properties but reader schema rejects them/.test(detail)
    || /^root keyword 'additionalProperties' \(schema form\) changed/.test(detail)) {
    return { reason: T('schemas.incompat.additional'), fix: generic };
  }
  const key = quoted(/^root keyword '([^']+)' changed/);
  if (key != null) return { reason: T('schemas.incompat.root_keyword', { key }), fix: generic };
  const def = quoted(/^definition '#\/([^']+)'/);
  if (def != null) return { reason: T('schemas.incompat.definition', { key: def }), fix: generic };
  if (/^root schema 'type' must include "object"/.test(detail)) return { reason: T('schemas.incompat.root_type'), fix: generic };
  return null;
}

/**
 * The refusal of an added pattern or version as markup: a bold first line
 * (`title`), what happened in words and — for a reason the screen could not
 * put in words, or a text the server could not read — the server's own
 * sentence in a folded block. `compatibility` is the pattern's.
 */
export function refusalHtml({ err, title, compatibility, schemaType, newText, describeError, offerDrop = false }) {
  const message = String(err?.message || err || '');
  const technical = (text) => `<details class="tb-tech"><summary>${escapeHtml(T('schemas.incompat.technical'))}</summary><pre>${escapeHtml(text)}</pre></details>`;
  const head = `<b>${escapeHtml(title)}</b>`;
  const incompatible = parseIncompatible(message);
  if (incompatible) {
    const compat = compatLabel(incompatible.mode || compatibility);
    const known = incompatibilityReason({ ...incompatible, newText });
    if (known) {
      // One click on a refused HL7 profile: take out what the old messages cannot satisfy and add the version.
      const drop = offerDrop && schemaType === 'hl7v2_profile' ? hl7DropOffer(incompatible.detail) : null;
      const dropButton = drop
        ? `<div class="tb-window-actions"><tf-button variant="primary" size="sm" icon="plus" data-act="drop-required" data-drop="${escapeAttr(JSON.stringify({ segments: drop.segments, fields: drop.fields }))}">${escapeHtml(T('schemas.version.drop_and_add', { items: listText(drop.items) }))}</tf-button></div>`
        : '';
      return `<div>${head} ${escapeHtml(T('schemas.incompat.lead', { compat, reason: known.reason }))} ${escapeHtml(known.fix)}</div>${dropButton}`;
    }
    return `<div>${head} ${escapeHtml(T('schemas.incompat.unknown', { compat }))} ${escapeHtml(T('schemas.incompat.fix_generic'))}</div>${technical(incompatible.detail)}`;
  }
  const invalid = /\bbus\.invalid_argument: schema: ([\s\S]*)$/.exec(message);
  if (invalid) {
    return `<div>${head} ${escapeHtml(T('schemas.text_refused', { format: schemaFormatLabel(schemaType) }))}</div>${technical(invalid[1].trim())}`;
  }
  if (/\bbus\.invalid_argument: subject '[^']*' is deprecated/.test(message)) {
    return `<div>${head} ${escapeHtml(T('schemas.version.refused_withdrawn'))}</div>`;
  }
  return `<div>${head} ${escapeHtml(describeError(err))}</div>`;
}

/** The topics the refusal of a delete names (`… is bound by topics: a, b`), or `null`. */
export function boundTopics(message) {
  const m = /is bound by topics: ([^\n]+)$/.exec(String(message || ''));
  return m ? m[1].split(',').map((t) => t.trim()).filter(Boolean) : null;
}

/** Which version topics check with: the newest not withdrawn, else the newest (`registry::effective_version`). */
export function effectiveVersion(subjectDeprecated, versions) {
  const sorted = [...(versions || [])].sort((a, b) => a.version - b.version);
  const latest = sorted[sorted.length - 1];
  if (!latest) return null;
  if (subjectDeprecated) return latest.version;
  const active = sorted.filter((v) => v.deprecatedAtMs == null);
  return (active[active.length - 1] || latest).version;
}

/** "topik wizyty" / "topiki a i b" — who checks with the pattern, for a sentence. */
function topicsPhrase(topics) {
  return T('schemas.topics_phrase', { topics: listText(topics), n: topics.length });
}

/** "Co się stanie po dodaniu" of a new version. */
export function versionImpact({ nextVersion, latestVersion, compatibility, usedByTopics }) {
  const lines = [T('schemas.version.impact_new', { version: fmtCount(nextVersion) })];
  if (compatibility !== 'none' && latestVersion != null) {
    lines.push(T('schemas.version.impact_check', { compat: compatLabel(compatibility), version: fmtCount(latestVersion) }));
  }
  const topics = [...(usedByTopics || [])].sort();
  lines.push(topics.length
    ? T('schemas.version.impact_topics', { topics: listText(topics), n: topics.length })
    : T('schemas.version.impact_no_topics'));
  return lines;
}

/** "Co się stanie po wycofaniu" of one version. */
export function versionDeprecateImpact({ version, versions, usedByTopics }) {
  const topics = [...(usedByTopics || [])].sort();
  const current = effectiveVersion(false, versions);
  const withdrawn = (versions || []).map((v) => (v.version === version ? { ...v, deprecatedAtMs: 0 } : v));
  const after = effectiveVersion(false, withdrawn);
  const allWithdrawn = withdrawn.every((v) => v.deprecatedAtMs != null);
  const who = topics.length ? topicsPhrase(topics) : null;
  if (allWithdrawn) {
    return [T(who ? 'schemas.withdraw_version.impact_last_used' : 'schemas.withdraw_version.impact_last', { who, n: topics.length, version: fmtCount(after) })];
  }
  if (version === current) {
    return [T(who ? 'schemas.withdraw_version.impact_moves_used' : 'schemas.withdraw_version.impact_moves', { who, n: topics.length, version: fmtCount(after) })];
  }
  return [T(who ? 'schemas.withdraw_version.impact_stays_used' : 'schemas.withdraw_version.impact_stays', { who, n: topics.length, version: fmtCount(current) })];
}

/** "Co się stanie po wycofaniu" of the whole pattern. */
export function subjectDeprecateImpact({ usedByTopics }) {
  const topics = [...(usedByTopics || [])].sort();
  const lines = [T('schemas.withdraw.impact')];
  if (topics.length) lines.push(T('schemas.withdraw.impact_topics', { topics: listText(topics), n: topics.length }));
  return lines;
}

/**
 * What "Dodaj wzór" did, from the server's answer: a new pattern, or — when a
 * pattern of that name appeared after the list was read — a version added to
 * it, or nothing at all when that version already had the same text.
 */
export function addedNotice({ subject, schemaType, version, deduplicated }) {
  if (deduplicated) return { tone: 'warning', title: T('schemas.added_existing_title', { name: subject }), text: T('schemas.added_existing_same', { version: fmtCount(version) }) };
  if (version > 1) return { tone: 'warning', title: T('schemas.added_existing_title', { name: subject }), text: T('schemas.added_existing_version', { version: fmtCount(version) }) };
  return {
    tone: 'success',
    title: T('schemas.added_title', { name: subject }),
    text: T('schemas.added_text', { format: schemaFormatLabel(schemaType), where: T(`schemas.kind_for.${schemaKind(schemaType) || 'binary'}`) }),
  };
}

// ---------------------------------------------------------------------------
// The windows
// ---------------------------------------------------------------------------

const TEXT_ERRORS = { empty: 'schemas.text_empty', too_big: 'schemas.text_too_big', not_json: 'schemas.text_not_json', not_xml: 'schemas.text_not_xml' };
const NAME_ERRORS = { empty: 'schemas.add.name_empty', invalid: 'schemas.add.name_invalid', taken: 'schemas.add.name_taken' };

function textField(label, hint, value = '') {
  return `
    <tf-textarea class="tb-mono-input" data-role="text" rows="10" label="${escapeAttr(label)}" hint="${escapeAttr(hint)}" value="${escapeAttr(value)}" spellcheck="false"></tf-textarea>
    <tf-file-input data-role="file" label="${escapeAttr(T('schemas.file_label'))}"></tf-file-input>`;
}

// Loads a picked file into the text field; a file over the size the server
// takes is refused here with the reason instead of being sent.
function wireTextField(win, sync, formatOf) {
  const text = win.querySelector('[data-role="text"]');
  const check = () => {
    const problem = text.value.trim() ? schemaTextProblem(text.value, formatOf()) : null;
    if (problem) text.setAttribute('error', T(TEXT_ERRORS[problem]));
    else text.removeAttribute('error');
  };
  text.addEventListener('input', () => { check(); sync(); });
  win.querySelector('[data-role="file"]').addEventListener('change', async (e) => {
    const file = e.detail?.files?.[0];
    if (!file) return;
    if (file.size > TEXT_MAX_BYTES) {
      text.setAttribute('error', T('schemas.text_too_big'));
      sync();
      return;
    }
    text.value = await file.text();
    check();
    sync();
  });
  return check;
}

// The option names are sentences themselves, so the list gets the whole row
// and its hint says what the chosen one checks.
const compatHint = (c) => `${T(`schemas.compat_desc.${c}`)} ${T('schemas.add.compat_hint')}`;

// A text field marked with an error (a file too big to load) holds nothing sendable.
const textMarked = (win) => win.querySelector('[data-role="text"]').hasAttribute('error');

/**
 * "Dodaj wzór". `ctx` = `{ instanceId, schemaTypes, existingNames(),
 * register(request), describeError, onAdded({ subject, schemaType, version,
 * deduplicated }) }` — `existingNames()` is asked on every check, so a list
 * reloaded while the window is open counts. The server's answer, not the
 * window, says what was added: a name taken meanwhile gets a version.
 */
export function openSchemaAdd(ctx) {
  const types = (ctx.schemaTypes || []).filter(Boolean);
  const initialType = types.includes('json_schema') ? 'json_schema' : (types[0] || '');
  const tiles = types.map((t) => `<tf-choice-card value="${escapeAttr(t)}" heading="${escapeAttr(schemaFormatLabel(t))}" description="${escapeAttr(T(`schemas.add.format_sub.${schemaKind(t) || 'binary'}`))}"${t === initialType ? ' selected' : ''}></tf-choice-card>`).join('');
  let check = () => {};
  return openChangeWindow({
    title: T('schemas.add.title'),
    icon: 'file-code',
    width: 700,
    cls: 'tb-schema-window',
    current: { subject: '', compatibility: DEFAULT_COMPATIBILITY, schemaType: initialType, schemaText: '' },
    saveLabel: T('schemas.add.button'),
    saveIcon: 'plus',
    willHappen: T('schemas.will_happen_add'),
    fields: () => `
      <tf-input data-role="name" class="tb-mono-input" autocomplete="off" spellcheck="false" label="${escapeAttr(T('schemas.add.name_label'))}" hint="${escapeAttr(T('schemas.add.name_hint'))}"></tf-input>
      <tf-select data-role="compat" label="${escapeAttr(T('schemas.col_compat'))}" hint="${escapeAttr(compatHint(DEFAULT_COMPATIBILITY))}"></tf-select>
      <div class="field">
        <label>${escapeHtml(T('schemas.col_format'))}</label>
        ${types.length
          ? `<tf-choice-group data-role="format" columns="3" value="${escapeAttr(initialType)}" aria-label="${escapeAttr(T('schemas.col_format'))}">${tiles}</tf-choice-group>`
          : `<div class="tb-explain-box">${escapeHtml(T('schemas.add.no_formats'))}</div>`}
      </div>
      ${textField(T('schemas.text_label'), T('schemas.add.text_hint'))}`,
    wire: (win, sync) => {
      const name = win.querySelector('[data-role="name"]');
      name.addEventListener('input', () => {
        const problem = name.value.trim() ? subjectNameProblem(name.value, ctx.existingNames()) : null;
        if (problem) name.setAttribute('error', T(NAME_ERRORS[problem]));
        else name.removeAttribute('error');
        sync();
      });
      const compat = win.querySelector('[data-role="compat"]');
      compat.setOptions(COMPATIBILITIES.map((c) => ({ value: c, label: T(`schemas.compat_title.${c}`) })), DEFAULT_COMPATIBILITY);
      compat.addEventListener('change', () => { compat.setAttribute('hint', compatHint(compat.value)); sync(); });
      const format = win.querySelector('[data-role="format"]');
      check = wireTextField(win, sync, () => format?.value || '');
      format?.addEventListener('change', () => { check(); sync(); });
    },
    draft: (win) => {
      const subject = win.querySelector('[data-role="name"]').value.trim();
      const schemaType = win.querySelector('[data-role="format"]')?.value || '';
      const schemaText = win.querySelector('[data-role="text"]').value;
      if (subjectNameProblem(subject, ctx.existingNames()) || !schemaType || schemaTextProblem(schemaText, schemaType) || textMarked(win)) return null;
      return { subject, compatibility: win.querySelector('[data-role="compat"]').value, schemaType, schemaText };
    },
    impact: (d) => [T('schemas.add.impact', { name: d.subject, format: schemaFormatLabel(d.schemaType) }), T('schemas.add.impact_unused')],
    save: async (d) => ctx.register(buildRegisterRequest(ctx.instanceId, d)),
    errorHtml: (err, d) => refusalHtml({
      err,
      title: T('schemas.add.refused', { name: d.subject }),
      compatibility: d.compatibility,
      schemaType: d.schemaType,
      newText: d.schemaText,
      describeError: ctx.describeError,
    }),
    describeError: ctx.describeError,
    onSaved: (d, resp) => ctx.onAdded({ subject: d.subject, schemaType: d.schemaType, version: Number(resp?.version) || 1, deduplicated: resp?.deduplicated === true }),
  });
}

/**
 * "Nowa wersja". `ctx` = `{ instanceId, subject: <list row>, latestText,
 * draftText, register(request), describeError, onAdded({ version,
 * deduplicated }), onDraft(text) }` — `draftText` is a refused text kept from
 * the last attempt (else the newest version's text is the starting point);
 * `onDraft` receives the text whenever the window closes without adding.
 */
export function openSchemaVersion(ctx) {
  const info = ctx.subject;
  const latest = Number(info.latestVersion) || 0;
  const next = latest + 1;
  const start = ctx.draftText ?? ctx.latestText ?? '';
  let added = false;
  const win = openChangeWindow({
    title: T('schemas.version.title', { name: info.subject }),
    icon: 'plus',
    width: 700,
    cls: 'tb-schema-window',
    current: { schemaText: ctx.latestText ?? '' },
    saveLabel: T('schemas.version.button'),
    saveIcon: 'plus',
    willHappen: T('schemas.will_happen_add'),
    // The text of a window closed without adding is kept for the next one (`onDraft`).
    discardText: T('schemas.version.discard_kept'),
    fields: () => `
      ${ctx.draftText != null ? `<div class="tb-explain-box">${escapeHtml(T('schemas.version.draft_kept'))}</div>` : ''}
      ${textField(T('schemas.version.text_label', { version: fmtCount(next) }), '', start)}
      <div class="tb-vr-hint" data-role="diff"></div>`,
    wire: (win, sync) => {
      const diff = win.querySelector('[data-role="diff"]');
      const paintDiff = () => {
        const changes = { json_schema: jsonSchemaChanges, hl7v2_profile: profileChanges }[info.schemaType];
        const text = changes ? changes(ctx.latestText, win.querySelector('[data-role="text"]').value, latest) : null;
        diff.textContent = text || '';
        diff.hidden = !text;
      };
      const check = wireTextField(win, () => { paintDiff(); sync(); }, () => info.schemaType);
      check();
      paintDiff();
    },
    draft: (win) => {
      const schemaText = win.querySelector('[data-role="text"]').value;
      return schemaTextProblem(schemaText, info.schemaType) || textMarked(win) ? null : { schemaText };
    },
    impact: () => versionImpact({ nextVersion: next, latestVersion: latest || null, compatibility: info.compatibility, usedByTopics: info.usedByTopics }),
    save: async (d) => ctx.register(buildRegisterRequest(ctx.instanceId, { subject: info.subject, schemaType: info.schemaType, schemaText: d.schemaText })),
    errorHtml: (err, d) => refusalHtml({
      err,
      title: T('schemas.version.refused', { version: fmtCount(next) }),
      compatibility: info.compatibility,
      schemaType: info.schemaType,
      newText: d.schemaText,
      describeError: ctx.describeError,
      offerDrop: true,
    }),
    describeError: ctx.describeError,
    onSaved: (d, resp) => {
      added = true;
      ctx.onAdded({ version: Number(resp?.version) || next, deduplicated: resp?.deduplicated === true });
    },
  });
  win.addEventListener('click', (e) => {
    const button = e.target.closest?.('[data-act="drop-required"]');
    if (!button) return;
    let drop;
    try {
      drop = JSON.parse(button.dataset.drop);
    } catch {
      return;
    }
    const field = win.querySelector('[data-role="text"]');
    const next = dropRequired(field.value, drop);
    if (next == null) return;
    field.value = next;
    field.dispatchEvent(new Event('input', { bubbles: true }));
    win.querySelector('[data-act="save"]')?.click();
  });
  win.addEventListener('closed', () => {
    if (added) return;
    const text = win.querySelector('[data-role="text"]')?.value;
    ctx.onDraft?.(text != null && text !== (ctx.latestText ?? '') ? text : null);
  });
  return win;
}

/**
 * "Zmień zgodność". `ctx` = `{ instanceId, subject: <list row>,
 * setCompatibility(request), describeError, onSaved(compatibility) }`.
 */
export function openSchemaCompat(ctx) {
  const info = ctx.subject;
  const current = { compatibility: info.compatibility };
  return openChangeWindow({
    title: T('schemas.compat_window.title', { name: info.subject }),
    icon: 'shield',
    width: 600,
    cls: 'tb-schema-window',
    current,
    fields: () => `
      <tf-radio-group cards data-role="compat" name="tb-schema-compat" value="${escapeAttr(info.compatibility)}" aria-label="${escapeAttr(T('schemas.col_compat'))}">
        ${COMPATIBILITIES.map((c) => `
          <tf-radio card value="${c}">
            <div><div class="tb-rc-name">${escapeHtml(T(`schemas.compat_title.${c}`))}</div><div class="tb-rc-sub">${escapeHtml(T(`schemas.compat_desc.${c}`))}</div></div>
          </tf-radio>`).join('')}
      </tf-radio-group>
      <div class="tb-vr-hint">${escapeHtml(T('settings.now', { value: compatLabel(info.compatibility) }))}</div>`,
    wire: (win, sync) => win.querySelector('[data-role="compat"]').addEventListener('change', sync),
    draft: (win) => ({ compatibility: win.querySelector('[data-role="compat"]').value }),
    impact: (d) => [T('schemas.compat_window.impact', { name: info.subject, compat: compatLabel(d.compatibility) })],
    save: (d) => ctx.setCompatibility({ instanceId: ctx.instanceId, subject: info.subject, compatibility: d.compatibility }),
    describeError: ctx.describeError,
    onSaved: (d) => ctx.onSaved(d.compatibility),
  });
}

/** "Wycofaj" the whole pattern. `ctx` = `{ instanceId, subject, versions, remove(request), describeError, onDone() }`. */
export function openSchemaDeprecate(ctx) {
  const info = ctx.subject;
  const versions = ctx.versions || [];
  const active = versions.filter((v) => v.deprecatedAtMs == null).map((v) => v.version).sort((a, b) => a - b);
  const lead = active.length === versions.length
    ? T('schemas.withdraw.lead_all', { name: info.subject, count: fmtCount(versions.length), n: versions.length })
    : T('schemas.withdraw.lead_some', { name: info.subject, count: fmtCount(active.length), n: active.length });
  return openConfirmWindow({
    title: T('schemas.withdraw.title', { name: info.subject }),
    icon: 'history',
    cls: 'tb-schema-window',
    width: 560,
    lead: `<div class="tb-explain-box">${escapeHtml(lead)} ${escapeHtml(T('schemas.withdraw.readable'))}</div>`,
    impactTitle: T('schemas.will_happen_withdraw'),
    impact: subjectDeprecateImpact(info),
    audit: T('schemas.audit_withdraw'),
    button: T('schemas.withdraw.button'),
    buttonIcon: 'history',
    run: () => ctx.remove(buildDeleteRequest(ctx.instanceId, info.subject, { deprecateOnly: true })),
    describeError: ctx.describeError,
    onDone: () => ctx.onDone(),
  });
}

/** "Wycofaj" one version. `ctx` = `{ instanceId, subject, version, versions, remove(request), describeError, onDone() }`. */
export function openVersionDeprecate(ctx) {
  const info = ctx.subject;
  const version = Number(ctx.version);
  return openConfirmWindow({
    title: T('schemas.withdraw_version.title', { version: fmtCount(version), name: info.subject }),
    icon: 'history',
    cls: 'tb-schema-window',
    width: 540,
    lead: `<div class="tb-explain-box">${escapeHtml(T('schemas.withdraw_version.lead', { version: fmtCount(version) }))}</div>`,
    impactTitle: T('schemas.will_happen_withdraw'),
    impact: versionDeprecateImpact({ version, versions: ctx.versions, usedByTopics: info.usedByTopics }),
    audit: T('schemas.audit_withdraw'),
    button: T('schemas.withdraw_version.button'),
    buttonIcon: 'history',
    run: () => ctx.remove(buildDeleteRequest(ctx.instanceId, info.subject, { version, deprecateOnly: true })),
    describeError: ctx.describeError,
    onDone: () => ctx.onDone(),
  });
}

/**
 * "Usuń…" a pattern no topic uses, confirmed by retyping its name. A refusal
 * because a topic took it meanwhile names the topics. `ctx` = `{ instanceId,
 * subject, remove(request), describeError, onDeleted() }`.
 */
export function openSchemaDelete(ctx) {
  const info = ctx.subject;
  const name = info.subject;
  return openRetypeDialog({
    title: T('schemas.delete.title', { name }),
    icon: 'alert',
    name,
    modal: true,
    className: 'tb-window tb-delete-window',
    width: 560,
    bodyHtml: `
      <div class="tb-danger-box">${sprite('alert')}<div><b>${escapeHtml(T('schemas.delete.irreversible'))}</b> ${escapeHtml(T('schemas.delete.everything', { name, format: schemaFormatLabel(info.schemaType) }))}</div></div>
      <ul class="tb-impact-list">
        <li>${sprite('check')}<span>${escapeHtml(T('schemas.delete.unused'))}</span></li>
        <li>${sprite('file-text')}<span>${escapeHtml(T('schemas.delete.audit'))}</span></li>
      </ul>`,
    retypeLabel: `${escapeHtml(T('schemas.delete.retype'))} <code class="tb-retype-name">${escapeHtml(name)}</code>`,
    confirmLabel: T('schemas.delete.button'),
    describeError: (err) => {
      const topics = boundTopics(err?.message);
      return topics
        ? T('schemas.delete.refused_used', { topics: listText(topics), n: topics.length })
        : ctx.describeError(err);
    },
    onConfirm: async () => {
      await ctx.remove(buildDeleteRequest(ctx.instanceId, name, { deprecateOnly: false }));
      queueMicrotask(() => ctx.onDeleted());
      return true;
    },
  });
}
