// ===== File: modules/tentabus/topic-hiding.js — a topic's Ukrywanie danych section: which fields each person, group or addon sees or may write =====
//
// The server stores a rule as the fields it ALLOWS (`BusFieldPolicyWire.fields`):
// one rule per subject and direction, a field that is not allowed is hidden on
// reading and refused on writing. The screen shows it the other way round —
// what is hidden — because that is what an administrator decides:
//   - reading: "Ukryj" takes a field out of the allowed list; the table lists
//     the known fields that are not allowed;
//   - writing: each field is allowed, required (`required_fields`, a subset of
//     the allowed ones) or not allowed; a message with a field that is not
//     allowed, or without a required one, is refused as a whole.
//
// What the server does, which the windows say before anyone confirms:
//   - a person's own rule wins over the rules of their groups, which win over
//     the rule for everyone; rules are never added together
//     (`field_policies::resolve`);
//   - a field the window does not list is not allowed either, so it is hidden
//     (or refused): the allowed list is the only thing stored;
//   - a general API key meets only the rule for everyone, and a topic with
//     rules for chosen subjects but none for everyone is closed to keys.
//
// The known fields come from the topic's message pattern (a JSON Schema's
// top-level properties) or, for HL7 v2, from a dictionary (hl7-fields.js); a
// topic with neither (XML, JSON without a pattern) is listed by hand. The
// shell (tentabus.js) loads the rules and the pattern; this module paints the
// section and builds the requests, the windows are in topic-hiding-windows.js.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml, setAttr, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtDayTime, contentKind } from '/js/modules/tentabus/format.js';
import { subjectTitle, subjectSub } from '/js/modules/tentabus/topic-access.js';
import { HL7_FIELDS, hl7FieldLabel } from '/js/modules/tentabus/hl7-fields.js';
import '/js/components/tf-button.js';
import '/js/components/tf-table.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

export const DIRECTIONS = ['read', 'write'];
/** Who a rule can name, in the order of the window. */
export const SUBJECT_KINDS = ['group', 'user', 'addon', 'any'];
/** The id the server stores for the rule that covers everyone. */
export const ANY_ID = '*';
/** Actions that take a field away from a reader, widest first; the server offers more over time (`fieldActions`). */
const READ_HIDING_ACTIONS = ['hide', 'mask', 'hash'];
/** A table cell names this many fields before it counts the rest. */
const CELL_FIELDS = 3;

const KIND_ORDER = { any: 0, group: 1, user: 2, addon: 3 };
const listFormat = (items) => new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(items);

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/** `read` / `write` / `any` — how a rule is addressed in a row key and in a request. */
export const policyKey = (p) => `${p.subjectType}:${p.subjectId}:${p.direction}`;

/**
 * One row per rule (subject and direction): `{ key, subjectType, subjectId,
 * direction, label, memberCount, fields, requiredFields, updatedAtMs }`.
 * Everyone first, then groups, people and addons, each by name; reading
 * before writing.
 */
export function policyRows(policies) {
  const rows = (policies || []).map((p) => ({
    key: policyKey(p),
    subjectType: p.subjectType,
    subjectId: p.subjectId,
    direction: p.direction,
    label: p.subjectType === 'any' ? null : (p.subjectLabel || null),
    memberCount: p.memberCount ?? null,
    fields: [...(p.fields || [])],
    requiredFields: [...(p.requiredFields || [])],
    updatedAtMs: p.updatedAtMs ?? null,
  }));
  const lang = I18n.getLanguage();
  return rows.sort((a, b) => (KIND_ORDER[a.subjectType] ?? 4) - (KIND_ORDER[b.subjectType] ?? 4)
    || Number(!a.label) - Number(!b.label)
    || String(a.label ?? a.subjectId).localeCompare(String(b.label ?? b.subjectId), lang)
    || DIRECTIONS.indexOf(a.direction) - DIRECTIONS.indexOf(b.direction));
}

/** The name a rule goes by: the person, group or addon, or "Wszyscy". */
export function whoTitle(row) {
  return row.subjectType === 'any' ? T('hiding.everyone') : subjectTitle(row);
}

/** The second line of the "Kto" cell: the kind, a group's size, what "Wszyscy" covers. */
export function whoSub(row) {
  return row.subjectType === 'any' ? T('hiding.everyone_sub') : subjectSub(row);
}

/** The rules shown in the menu's counter, `null` while they load. */
export function hidingCount(data) {
  return data?.policies ? data.policies.length : null;
}

// ---------------------------------------------------------------------------
// The fields a rule can name
// ---------------------------------------------------------------------------

/** What the payload of a topic is, as the server's field codecs see it: `json`, `xml`, `hl7v2` or `binary`. */
export function topicFormat(contentType) {
  return contentKind(contentType) || 'json';
}

const clip = (text, max = 70) => (text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text);

/**
 * The top-level properties of a JSON Schema as `[{ name, label }]`, in the
 * order the pattern writes them; the label is the property's title, else the
 * start of its description. Properties of an `allOf` branch count too. An
 * empty list for text that is not a schema with properties.
 */
export function jsonSchemaFields(text) {
  let root;
  try {
    root = JSON.parse(text);
  } catch {
    return [];
  }
  const found = new Map();
  const collect = (node) => {
    if (!node || typeof node !== 'object') return;
    const props = node.properties;
    if (props && typeof props === 'object' && !Array.isArray(props)) {
      for (const [name, def] of Object.entries(props)) {
        if (found.has(name)) continue;
        const label = def && typeof def === 'object' ? String(def.title || def.description || '').trim() : '';
        found.set(name, { name, label: clip(label) });
      }
    }
    if (Array.isArray(node.allOf)) node.allOf.forEach(collect);
  };
  collect(root);
  return [...found.values()];
}

/**
 * Where a topic's known fields come from. `schema` = `{ subject, version,
 * text }` of the pattern the topic checks with, `{ failed: true }` when it
 * could not be read, or `null`. Returns `{ mode, fields, subject, version,
 * failed }` with `mode`:
 *   - `schema`: the pattern's properties;
 *   - `dictionary`: HL7 v2's common fields (any other address can be typed);
 *   - `manual`: no list — the fields to keep are typed in;
 *   - `blocked`: binary content, which has no fields to name.
 */
export function fieldSource({ format, schema }) {
  if (format === 'binary') return { mode: 'blocked', fields: [] };
  if (format === 'hl7v2') {
    return { mode: 'dictionary', fields: HL7_FIELDS.map((name) => ({ name, label: hl7FieldLabel(name) })) };
  }
  if (format === 'json' && schema && !schema.failed) {
    const fields = jsonSchemaFields(schema.text);
    if (fields.length) return { mode: 'schema', fields, subject: schema.subject, version: schema.version };
  }
  return { mode: 'manual', fields: [], failed: Boolean(schema?.failed) };
}

/** The plain name of a field in `source`, '' when the source has none. */
export function fieldLabel(source, name) {
  return source.fields.find((f) => f.name === name)?.label || hl7FieldLabel(name) || '';
}

/** "PESEL pacjenta (pacjent.pesel)", or the bare address when the field has no name. */
export function fieldPhrase(source, name) {
  const label = fieldLabel(source, name);
  return label ? `${label} (${name})` : name;
}

const HL7_ADDRESS = /^[A-Z0-9]{3}-[1-9][0-9]*$/;
const XML_NAME = /^[\p{Alphabetic}_][\p{Alphabetic}\p{N}_.:-]*$/u;

/**
 * Why `name` cannot be a field address of this format, as the key of the
 * reason (`hiding.field_problem.<key>`), or `null`. The server checks the same
 * rules (`PayloadFieldFormat::validate_field_name`); this only says so before
 * the request.
 */
export function fieldNameProblem(format, name) {
  const value = String(name ?? '');
  if (!value.trim()) return 'empty';
  if (format === 'hl7v2') {
    if (value === 'MSH-1' || value === 'MSH-2') return 'hl7_msh';
    return HL7_ADDRESS.test(value) ? null : 'hl7_shape';
  }
  if (format === 'xml') return XML_NAME.test(value) ? null : 'xml_name';
  return null;
}

// ---------------------------------------------------------------------------
// What a rule does, in words
// ---------------------------------------------------------------------------

/**
 * What a rule leaves out of the known fields: `{ known, hidden, forbidden,
 * required, visible }`. `known` says whether the source lists fields at all;
 * without a list only the allowed fields are known (`visible`).
 */
export function ruleFacts(row, source) {
  const names = source.fields.map((f) => f.name);
  const allowed = new Set(row.fields);
  const outside = names.filter((n) => !allowed.has(n));
  return {
    known: names.length > 0,
    hidden: row.direction === 'read' ? outside : [],
    forbidden: row.direction === 'write' ? outside : [],
    required: row.direction === 'write' ? [...row.requiredFields].sort() : [],
    visible: [...row.fields].sort(),
  };
}

const chipHtml = (tone, text) => `<span class="tf-chip tf-chip--outline ${tone}">${escapeHtml(text)}</span>`;

function fieldLineHtml(source, name, chip) {
  const label = fieldLabel(source, name);
  return `<div><span class="tf-table__cell--mono">${escapeHtml(name)}</span> ${chip}</div>${label ? `<div class="tf-table__cell-sub">${escapeHtml(label)}</div>` : ''}`;
}

function limitedHtml(source, items) {
  const shown = items.slice(0, CELL_FIELDS).map(([name, chip]) => fieldLineHtml(source, name, chip)).join('');
  const rest = items.length - CELL_FIELDS;
  return rest > 0
    ? `${shown}<div class="tf-table__cell-sub">${escapeHtml(T('hiding.and_more', { count: fmtCount(rest), n: rest }))}</div>`
    : shown;
}

/** The "Co widzi inaczej" cell of a rule: the fields it hides (or refuses, or requires), one line each. */
export function ruleWhatHtml(row, source) {
  const facts = ruleFacts(row, source);
  const plain = (text) => `<span>${escapeHtml(text)}</span>`;
  if (row.direction === 'read') {
    if (!facts.known) {
      return plain(facts.visible.length ? T('hiding.sees_only', { fields: facts.visible.join(', ') }) : T('hiding.sees_nothing'));
    }
    if (!facts.hidden.length) return plain(T('hiding.hides_none'));
    return limitedHtml(source, facts.hidden.map((n) => [n, chipHtml('neutral', T('hiding.action.hide'))]));
  }
  const requiredItems = facts.required.map((n) => [n, chipHtml('ok', T('hiding.write.require'))]);
  if (!facts.known) {
    const only = plain(facts.visible.length ? T('hiding.writes_only', { fields: facts.visible.join(', ') }) : T('hiding.writes_nothing'));
    return requiredItems.length ? `${only}${limitedHtml(source, requiredItems)}` : only;
  }
  const items = [...requiredItems, ...facts.forbidden.map((n) => [n, chipHtml('err', T('hiding.write.forbid'))])];
  return items.length ? limitedHtml(source, items) : plain(T('hiding.writes_all'));
}

/** A table row of the rules' card. */
export function ruleTableRow(row, source, nowMs = Date.now()) {
  return {
    who: `<span class="tf-table__cell-title">${escapeHtml(whoTitle(row))}</span><div class="tf-table__cell-sub">${escapeHtml(whoSub(row))}</div>`,
    what: ruleWhatHtml(row, source),
    when: chipHtml('neutral', T(`hiding.direction.${row.direction}`)),
    changed: escapeHtml(fmtDayTime(row.updatedAtMs, nowMs)),
    _key: row.key,
  };
}

/** Actions a reader's field can be set to: "Pokaż" and what `fieldActions` says the server can do besides. */
export function readActionList(fieldActions) {
  const offered = new Set(fieldActions || []);
  return ['show', ...READ_HIDING_ACTIONS.filter((a) => a === 'hide' || offered.has(a))];
}

/** The segmented options of one field in a window: reading offers "Pokaż" and the hiding actions, writing allowed / required / not allowed. */
export function actionOptions(direction, fieldActions) {
  const variants = { show: 'ok', hide: 'warn', mask: 'warn', hash: 'warn', allow: 'ok', require: 'accent', forbid: 'err' };
  const values = direction === 'read' ? readActionList(fieldActions) : ['allow', 'require', 'forbid'];
  return values.map((value) => ({
    value,
    label: T(direction === 'read' ? `hiding.action.${value}` : `hiding.write.${value}`),
    variant: variants[value],
  }));
}

/** Whether a reading action takes the field away from the reader (everything but "Pokaż"). */
const takesFieldAway = (action) => action !== 'show';

// ---------------------------------------------------------------------------
// A rule as the window edits it, and the request that stores it
// ---------------------------------------------------------------------------

const defaultAction = (direction) => (direction === 'read' ? 'show' : 'allow');
const sortedUnique = (list) => [...new Set(list)].sort();

/** The form of a new rule: every known field shown (reading) or allowed (writing), nothing typed in. */
export function blankForm(direction, source) {
  return { actions: Object.fromEntries(source.fields.map((f) => [f.name, defaultAction(direction)])), extraShown: [], extraRequired: [] };
}

/**
 * The form of a stored rule. A known field not among the allowed ones is
 * "Ukryj" (or "Niedozwolone"); the allowed ones outside the known list are
 * the typed-in fields.
 */
export function formFromRule(row, source) {
  const names = source.fields.map((f) => f.name);
  const known = new Set(names);
  const allowed = new Set(row.fields);
  const required = new Set(row.requiredFields);
  const actions = {};
  for (const n of names) {
    if (row.direction === 'read') actions[n] = allowed.has(n) ? 'show' : 'hide';
    else actions[n] = required.has(n) ? 'require' : allowed.has(n) ? 'allow' : 'forbid';
  }
  return {
    actions,
    extraShown: sortedUnique(row.fields.filter((n) => !known.has(n) && !(row.direction === 'write' && required.has(n)))),
    extraRequired: row.direction === 'write' ? sortedUnique(row.requiredFields.filter((n) => !known.has(n))) : [],
  };
}

/** A form as one string, so a window can tell whether anything changed. */
export function serializeForm(form, source) {
  return JSON.stringify([source.fields.map((f) => form.actions[f.name] ?? ''), sortedUnique(form.extraShown), sortedUnique(form.extraRequired)]);
}

/** The form a `serializeForm` string holds (`null` for text that is not one). */
export function parseForm(text, source) {
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed) || parsed.length !== 3) return null;
  const [actions, extraShown, extraRequired] = parsed;
  return {
    actions: Object.fromEntries(source.fields.map((f, i) => [f.name, actions[i] ?? ''])),
    extraShown: [...extraShown],
    extraRequired: [...extraRequired],
  };
}

/**
 * The `FieldPolicySetRequest` of a form. The allowed fields are the known
 * ones left shown (or allowed or required) plus the typed-in ones; a field
 * set to any action that takes it away — "Ukryj", and "Zamaskuj" or "Zahaszuj"
 * where the server offers them — is never listed as allowed, so a server that
 * does not store those actions yet still hides the field.
 */
export function buildPolicyRequest({ instanceId, topic, subject, direction, form, source }) {
  const names = source.fields.map((f) => f.name);
  const known = new Set(names);
  const typed = (list) => sortedUnique(list.map((n) => n.trim()).filter((n) => n && !known.has(n)));
  const read = direction === 'read';
  const allowedKnown = names.filter((n) => (read ? !takesFieldAway(form.actions[n]) : form.actions[n] !== 'forbid'));
  const extraRequired = read ? [] : typed(form.extraRequired);
  const requiredKnown = read ? [] : names.filter((n) => form.actions[n] === 'require');
  return {
    instanceId,
    topic,
    subjectType: subject.subjectType,
    subjectId: subject.subjectId,
    direction,
    fields: sortedUnique([...allowedKnown, ...typed(form.extraShown), ...extraRequired]),
    requiredFields: sortedUnique([...requiredKnown, ...extraRequired]),
  };
}

/**
 * What a form still lacks to be a rule worth storing, in words, or `null`:
 * a typed-in address the format refuses, or no field set to anything. A rule
 * that hides nothing does nothing — the way to switch one off is to delete it.
 */
export function formProblem({ direction, form, source, format }) {
  const typed = [...form.extraShown, ...form.extraRequired];
  for (const name of typed) {
    const reason = fieldNameProblem(format, name);
    if (reason) return T(`hiding.field_problem.${reason}`, { name });
  }
  const listed = source.fields.length > 0;
  if (direction === 'read') {
    if (listed) return source.fields.some((f) => takesFieldAway(form.actions[f.name])) ? null : T('hiding.need.read');
    return form.extraShown.length ? null : T('hiding.need.read_typed');
  }
  if (listed) {
    const set = source.fields.some((f) => form.actions[f.name] === 'forbid' || form.actions[f.name] === 'require');
    return set || form.extraRequired.length ? null : T('hiding.need.write');
  }
  return typed.length ? null : T('hiding.need.write_typed');
}

const phrases = (source, names) => names.map((n) => fieldPhrase(source, n)).join(', ');

/**
 * The sentences of "Co się stanie po zapisaniu". `who` = the name the rule
 * goes by; `current` = the form of the rule as it is stored (`null` for a
 * new rule). Fields are named, never counted, so the sentences hold in every
 * case; what stays unchanged is said only when something else changes.
 */
export function ruleImpact({ who, direction, form, current, source }) {
  const names = source.fields.map((f) => f.name);
  const lines = [];
  if (direction === 'read') {
    const away = (f) => names.filter((n) => takesFieldAway(f?.actions[n] ?? 'show'));
    const nowAway = away(form);
    const wasAway = new Set(away(current));
    const hiddenMore = nowAway.filter((n) => !wasAway.has(n));
    const shownMore = [...wasAway].filter((n) => !nowAway.includes(n));
    const typed = sortedUnique(form.extraShown);
    const wasTyped = new Set(current?.extraShown || []);
    const typedMore = typed.filter((n) => !wasTyped.has(n));
    const typedLess = [...wasTyped].filter((n) => !typed.includes(n));
    if (!names.length) return [T('hiding.impact.read_only', { who, fields: typed.join(', ') })];
    if (hiddenMore.length || typedLess.length) lines.push(T('hiding.impact.read_hidden', { who, fields: phrases(source, [...hiddenMore, ...typedLess]) }));
    // A new rule hides; the fields it leaves shown were shown before it.
    if (current && (shownMore.length || typedMore.length)) lines.push(T('hiding.impact.read_shown', { who, fields: phrases(source, [...shownMore, ...typedMore]) }));
    if (lines.length) lines.push(T('hiding.impact.rest'), T('hiding.impact.read_unlisted'));
    return lines;
  }
  const forbidden = names.filter((n) => form.actions[n] === 'forbid');
  const required = [...names.filter((n) => form.actions[n] === 'require'), ...sortedUnique(form.extraRequired)];
  if (!names.length) {
    lines.push(T('hiding.impact.write_only', { who, fields: sortedUnique([...form.extraShown, ...form.extraRequired]).join(', ') }));
  } else if (forbidden.length) {
    lines.push(T('hiding.impact.write_forbidden', { who, fields: phrases(source, forbidden) }));
  }
  if (required.length) lines.push(T('hiding.impact.write_required', { who, fields: phrases(source, required) }));
  if (lines.length) lines.push(T('hiding.impact.write_rest'));
  if (names.length && lines.length) lines.push(T('hiding.impact.write_unlisted'));
  return lines;
}

// ---------------------------------------------------------------------------
// The section
// ---------------------------------------------------------------------------

function skeleton() {
  return `
    <div data-role="notice"></div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('shield')} ${escapeHtml(T('hiding.title'))} <span data-role="count"></span></div>
        <div class="actions">
          <tf-button variant="secondary" size="sm" icon="eye" data-go="hiding-preview" data-role="preview">${escapeHtml(T('hiding.preview.button'))}</tf-button>
          <tf-button variant="primary" size="sm" icon="plus" data-go="hiding-add" data-role="add">${escapeHtml(T('hiding.add.button'))}</tf-button>
        </div>
      </div>
      <div class="section-sub">${escapeHtml(T('hiding.explain'))}</div>
      <div data-role="state"></div>
      <div data-role="keys-note"></div>
      <tf-table data-role="rules">
        <tf-column key="who" label="${escapeAttr(T('hiding.col_who'))}" renderer="html"></tf-column>
        <tf-column key="what" label="${escapeAttr(T('hiding.col_what'))}" renderer="html" fill></tf-column>
        <tf-column key="when" label="${escapeAttr(T('hiding.col_when'))}" renderer="html"></tf-column>
        <tf-column key="changed" label="${escapeAttr(T('hiding.col_changed'))}" renderer="html"></tf-column>
      </tf-table>
      <div class="tb-table-footer" data-role="legend"></div>
    </div>`;
}

function rowButtons(pairs) {
  const wrap = document.createElement('div');
  wrap.className = 'tf-table__row-actions';
  for (const [act, label, onClick] of pairs) {
    const b = document.createElement('tf-button');
    b.setAttribute('variant', 'secondary');
    b.setAttribute('size', 'sm');
    b.dataset.act = act;
    b.textContent = label;
    b.addEventListener('click', (e) => { e.stopPropagation(); onClick(); });
    wrap.appendChild(b);
  }
  return wrap;
}

function stateHtml({ loading, error }) {
  if (error) return `<div class="tb-state tb-state--error">${sprite('alert')}<span>${escapeHtml(error)}</span><tf-button variant="secondary" size="sm" icon="refresh" data-go="hiding-reload">${escapeHtml(T('shell.retry'))}</tf-button></div>`;
  if (loading) return `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`;
  return '';
}

/** The legend under the table: what a field can become; "zamaskuj" and "zahaszuj" only where the server offers them. */
export function legendHtml(fieldActions) {
  const offered = readActionList(fieldActions).filter((a) => a !== 'show');
  const items = [
    ...offered.map((a) => `<span><b>${escapeHtml(T(`hiding.action.${a}`))}</b> — ${escapeHtml(T(`hiding.legend.${a}`))}</span>`),
    `<span><b>${escapeHtml(T('hiding.legend.reject_name'))}</b> — ${escapeHtml(T('hiding.legend.reject'))}</span>`,
    `<span>${escapeHtml(T('hiding.legend.precedence'))}</span>`,
  ];
  return items.join('');
}

/**
 * Directions that have rules for chosen subjects and none for everyone: a
 * system with an API key meets only the rule for everyone, so the topic is
 * closed to keys for that direction (`bus.key_needs_topic_wide_rule`).
 */
export function directionsClosedToKeys(rows) {
  return DIRECTIONS.filter((d) => {
    const own = rows.filter((r) => r.direction === d);
    return own.length > 0 && !own.some((r) => r.subjectType === 'any');
  });
}

/**
 * Paints the Ukrywanie danych section. `view` carries the page's `topic`
 * (`name`, `contentType`), `access`, `capabilities` (`fieldActions`),
 * `notice`, `nowMs` and `hidingData` = `{ policies, policiesError, schema,
 * schemaSettled }` (`policies` null while loading; `schema` as in
 * `fieldSource`; `schemaSettled` false while the pattern is being read).
 * Buttons call `ctx.go({ kind: 'hiding-add' | 'hiding-preview' |
 * 'hiding-reload' })` and `{ kind: 'hiding-change' | 'hiding-remove', rule }`.
 */
export function paintHidingSection(host, view, ctx) {
  if (host.__tbHiding !== 'built') {
    host.__tbHiding = 'built';
    patchHtml(host, skeleton());
  }
  const data = view.hidingData || {};
  const source = fieldSource({ format: topicFormat(view.topic.contentType), schema: data.schema ?? null });
  const rows = data.policies ? policyRows(data.policies) : [];
  const loaded = Boolean(data.policies);
  const ready = loaded && data.schemaSettled !== false;
  patchHtml(host.querySelector('[data-role="notice"]'), view.notice
    ? `<tf-alert tone="${escapeAttr(view.notice.tone || 'success')}" title="${escapeAttr(view.notice.title)}" message="${escapeAttr(view.notice.text || '')}"></tf-alert>`
    : '');

  const count = host.querySelector('[data-role="count"]');
  if (loaded) {
    patchHtml(count, '<tf-chip size="sm" variant="outline" status="neutral"></tf-chip>');
    setAttr(count.firstElementChild, 'label', fmtCount(rows.length));
  } else {
    patchHtml(count, '');
  }

  const blocked = source.mode === 'blocked';
  let state = stateHtml({ loading: !loaded && !data.policiesError, error: loaded ? null : data.policiesError });
  if (loaded && !rows.length) {
    state = blocked
      ? `<tf-empty-state badge icon="shield" title="${escapeAttr(T('hiding.blocked_title'))}" message="${escapeAttr(T('hiding.blocked_text'))}"></tf-empty-state>`
      : `<tf-empty-state badge icon="shield" title="${escapeAttr(T('hiding.empty_title'))}" message="${escapeAttr(T('hiding.empty_text'))}"><tf-button variant="primary" icon="plus" data-go="hiding-add"${ready ? '' : ' disabled'}>${escapeHtml(T('hiding.add.button'))}</tf-button></tf-empty-state>`;
  }
  patchHtml(host.querySelector('[data-role="state"]'), state);

  const closed = directionsClosedToKeys(rows);
  patchHtml(host.querySelector('[data-role="keys-note"]'), closed.length
    ? `<div class="tb-who-can">${sprite('lock')}<span>${escapeHtml(T('hiding.keys_closed', { directions: listFormat(closed.map((d) => T(`hiding.direction_of.${d}`))) }))}</span></div>`
    : '');

  const add = host.querySelector('[data-role="add"]');
  setAttr(add, 'disabled', !ready || blocked);
  setAttr(add, 'title', blocked ? T('hiding.blocked_text') : null);
  const preview = host.querySelector('[data-role="preview"]');
  setAttr(preview, 'disabled', !view.access?.canRead || !loaded);
  setAttr(preview, 'title', view.access?.canRead ? null : T('detail.preview_no_read', { name: view.topic.name }));

  const table = host.querySelector('[data-role="rules"]');
  table.hidden = rows.length === 0;
  host.querySelector('[data-role="legend"]').hidden = rows.length === 0;
  if (!table.__tbWired) {
    table.__tbWired = true;
    table.rowActionsKey = (row) => row._key;
    table.rowActions = (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      return rowButtons([
        ['change', T('hiding.change.button'), () => ctx.go({ kind: 'hiding-change', rule: live()._key })],
        ['remove', T('hiding.remove.button'), () => ctx.go({ kind: 'hiding-remove', rule: live()._key })],
      ]);
    };
  }
  patchHtml(host.querySelector('[data-role="legend"]'), legendHtml(view.capabilities?.fieldActions));
  setRowsIfChanged(table, rows.map((r) => ruleTableRow(r, source, view.nowMs ?? Date.now())));
}
