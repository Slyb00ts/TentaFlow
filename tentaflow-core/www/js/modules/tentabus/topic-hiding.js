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
// properties, followed through allOf / oneOf / anyOf / if-then-else and local
// $ref) or, for HL7 v2, from a dictionary (hl7-fields.js) whose unnamed
// positions are allowed by default; a topic with neither (XML, JSON without a
// pattern, a pattern the screen cannot read to the end) is listed by hand. The
// shell (tentabus.js) loads the rules and the pattern; this module paints the
// section and builds the requests, the windows are in topic-hiding-windows.js.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml, setAttr, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtDayTime, contentKind } from '/js/modules/tentabus/format.js';
import { subjectTitle, subjectSub } from '/js/modules/tentabus/topic-access.js';
import { HL7_FIELDS, HL7_IMPLICIT_FIELDS, hl7FieldLabel } from '/js/modules/tentabus/hl7-fields.js';
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
 * Reads the fields of a JSON Schema: `{ fields: [{ name, label }], complex,
 * closed }`. The fields are the properties of the schema, its allOf / oneOf /
 * anyOf branches, if / then / else and every local `$ref` (`#/$defs/..`,
 * `#/definitions/..`), in the order the pattern writes them; the label is the
 * property's title, else the start of its description. `complex` is true when
 * the pattern names fields this walk cannot list — a remote or dangling
 * `$ref`, `patternProperties`, an `additionalProperties` schema — so the list
 * would be incomplete; `closed` when the schema forbids any other property.
 * No fields for text that is not a schema.
 */
export function readJsonSchema(text) {
  let root;
  try {
    root = JSON.parse(text);
  } catch {
    return { fields: [], complex: false, closed: false };
  }
  const found = new Map();
  const seen = new Set();
  let complex = false;
  const isObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
  const resolve = (ref) => {
    if (ref === '#') return root;
    if (typeof ref !== 'string' || !ref.startsWith('#/')) return undefined;
    let node = root;
    for (const part of ref.slice(2).split('/')) {
      let key;
      try {
        key = decodeURIComponent(part).replace(/~1/g, '/').replace(/~0/g, '~');
      } catch {
        return undefined;
      }
      if (!isObject(node) && !Array.isArray(node)) return undefined;
      if (!Object.hasOwn(node, key)) return undefined;
      node = node[key];
    }
    return node;
  };
  const collect = (node) => {
    if (!isObject(node) || seen.has(node)) return;
    seen.add(node);
    if ('$ref' in node) {
      const target = resolve(node.$ref);
      if (target === undefined) complex = true;
      else collect(target);
    }
    if (isObject(node.properties)) {
      for (const [name, def] of Object.entries(node.properties)) {
        if (found.has(name)) continue;
        const label = isObject(def) ? String(def.title || def.description || '').trim() : '';
        found.set(name, { name, label: clip(label) });
      }
    }
    if (isObject(node.patternProperties) && Object.keys(node.patternProperties).length) complex = true;
    if (isObject(node.additionalProperties)) complex = true;
    for (const key of ['allOf', 'oneOf', 'anyOf']) {
      if (Array.isArray(node[key])) node[key].forEach(collect);
    }
    for (const key of ['if', 'then', 'else']) collect(node[key]);
  };
  collect(root);
  return { fields: [...found.values()], complex, closed: isObject(root) && root.additionalProperties === false };
}

/**
 * Where a topic's known fields come from. `schema` = `{ subject, version,
 * text }` of the pattern the topic checks with, `{ failed: true }` when it
 * could not be read, or `null`. Returns `{ mode, fields, complete, implicit,
 * subject, version, failed, complex }` with `mode`:
 *   - `schema`: the pattern's properties;
 *   - `dictionary`: HL7 v2's common fields (any other address can be typed),
 *     with `implicit` = the unnamed positions of the same segments, which a
 *     new rule allows without a row of their own;
 *   - `manual`: no list — the fields to keep are typed in (`complex` = the
 *     pattern is there but names fields the screen cannot list);
 *   - `blocked`: binary content, which has no fields to name.
 * `complete` says the list is every field a message can carry (a closed
 * pattern): only then can a rule that leaves every listed field alone be a
 * rule that does nothing.
 */
export function fieldSource({ format, schema }) {
  if (format === 'binary') return { mode: 'blocked', fields: [], implicit: [], complete: false };
  if (format === 'hl7v2') {
    return { mode: 'dictionary', fields: HL7_FIELDS.map((name) => ({ name, label: hl7FieldLabel(name) })), implicit: HL7_IMPLICIT_FIELDS, complete: false };
  }
  let complex = false;
  if (format === 'json' && schema && !schema.failed) {
    const read = readJsonSchema(schema.text);
    if (read.fields.length && !read.complex) {
      return { mode: 'schema', fields: read.fields, implicit: [], complete: read.closed, subject: schema.subject, version: schema.version };
    }
    complex = read.complex;
  }
  return { mode: 'manual', fields: [], implicit: [], complete: false, failed: Boolean(schema?.failed), complex };
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
const HL7_LOOSE_ADDRESS = /^([A-Za-z0-9]{3})-?([1-9][0-9]*)$/;
const XML_NAME = /^[\p{Alphabetic}_][\p{Alphabetic}\p{N}_.:-]*$/u;

/** The address a near miss ("pid5", "pid-5") was probably meant to be ("PID-5"), or `null`. */
export function hl7Suggestion(name) {
  const m = HL7_LOOSE_ADDRESS.exec(String(name ?? '').trim());
  const suggestion = m ? `${m[1].toUpperCase()}-${m[2]}` : null;
  return suggestion && suggestion !== 'MSH-1' && suggestion !== 'MSH-2' ? suggestion : null;
}

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
    if (HL7_ADDRESS.test(value)) return null;
    return hl7Suggestion(value) ? 'hl7_case' : 'hl7_shape';
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
  const names = [...source.fields.map((f) => f.name), ...source.implicit];
  const allowed = new Set(row.fields);
  const outside = names.filter((n) => !allowed.has(n));
  return {
    known: source.fields.length > 0,
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
  const refused = `<div class="tf-table__cell-sub">${escapeHtml(T('hiding.writes_rest'))}</div>`;
  if (!facts.known) {
    const only = plain(facts.visible.length ? T('hiding.writes_only', { fields: facts.visible.join(', ') }) : T('hiding.writes_nothing'));
    return `${only}${requiredItems.length ? limitedHtml(source, requiredItems) : ''}${refused}`;
  }
  const items = [...requiredItems, ...facts.forbidden.map((n) => [n, chipHtml('err', T('hiding.write.forbid'))])];
  return `${items.length ? limitedHtml(source, items) : plain(T('hiding.writes_all'))}${refused}`;
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

/** The segmented options of one field in a window: reading offers "Pokaż" and "Ukryj", writing allowed / required / not allowed. */
export function actionOptions(direction) {
  const options = direction === 'read'
    ? [['show', 'hiding.action.show', 'ok'], ['hide', 'hiding.action.hide', 'warn']]
    : [['allow', 'hiding.write.allow', 'ok'], ['require', 'hiding.write.require', 'accent'], ['forbid', 'hiding.write.forbid', 'err']];
  return options.map(([value, key, variant]) => ({ value, label: T(key), variant }));
}

// ---------------------------------------------------------------------------
// A rule as the window edits it, and the request that stores it
// ---------------------------------------------------------------------------

const defaultAction = (direction) => (direction === 'read' ? 'show' : 'allow');
const sortedUnique = (list) => [...new Set(list)].sort();

/**
 * The form of a new rule: every known field shown (reading) or allowed
 * (writing), nothing typed in, no unnamed position kept out. `hiddenImplicit`
 * holds the unnamed positions a stored rule leaves out: the window has no row
 * for them, but saving the rule again must not allow them.
 */
export function blankForm(direction, source) {
  return { actions: Object.fromEntries(source.fields.map((f) => [f.name, defaultAction(direction)])), extraShown: [], extraRequired: [], hiddenImplicit: [] };
}

/**
 * The form of a stored rule. A known field not among the allowed ones is
 * "Ukryj" (or "Niedozwolone"); the allowed ones outside the known list are
 * the typed-in fields (an unnamed position of the dictionary's segments is
 * not typed in: it is allowed by default).
 */
export function formFromRule(row, source) {
  const names = source.fields.map((f) => f.name);
  const known = new Set(names);
  const implicit = new Set(source.implicit);
  const allowed = new Set(row.fields);
  const required = new Set(row.requiredFields);
  const actions = {};
  for (const n of names) {
    if (row.direction === 'read') actions[n] = allowed.has(n) ? 'show' : 'hide';
    else actions[n] = required.has(n) ? 'require' : allowed.has(n) ? 'allow' : 'forbid';
  }
  return {
    actions,
    extraShown: sortedUnique(row.fields.filter((n) => !known.has(n) && !implicit.has(n) && !(row.direction === 'write' && required.has(n)))),
    extraRequired: row.direction === 'write' ? sortedUnique(row.requiredFields.filter((n) => !known.has(n))) : [],
    hiddenImplicit: source.implicit.filter((n) => !allowed.has(n)),
  };
}

/** A form as one string, so a window can tell whether anything changed. */
export function serializeForm(form, source) {
  return JSON.stringify([
    source.fields.map((f) => form.actions[f.name] ?? ''),
    sortedUnique(form.extraShown),
    sortedUnique(form.extraRequired),
    sortedUnique(form.hiddenImplicit || []),
  ]);
}

/** The form a `serializeForm` string holds (`null` for text that is not one). */
export function parseForm(text, source) {
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed) || parsed.length !== 4) return null;
  const [actions, extraShown, extraRequired, hiddenImplicit] = parsed;
  return {
    actions: Object.fromEntries(source.fields.map((f, i) => [f.name, actions[i] ?? ''])),
    extraShown: [...extraShown],
    extraRequired: [...extraRequired],
    hiddenImplicit: [...hiddenImplicit],
  };
}

/**
 * The form as the request will carry it: the typed-in names trimmed, a typed
 * "required" name that is also a listed field makes that row "Wymagane", and
 * what stays typed is only what the list does not have. One place decides
 * this, so the request and the sentences about it cannot disagree.
 */
function effectiveForm(form, source, direction) {
  const known = new Set(source.fields.map((f) => f.name));
  const typed = (list) => sortedUnique(list.map((n) => n.trim()).filter(Boolean));
  const required = direction === 'read' ? [] : typed(form.extraRequired);
  const actions = { ...form.actions };
  for (const n of required) if (known.has(n) && actions[n] !== 'forbid') actions[n] = 'require';
  return {
    actions,
    extraShown: typed(form.extraShown).filter((n) => !known.has(n)),
    extraRequired: required.filter((n) => !known.has(n)),
    hiddenImplicit: form.hiddenImplicit || [],
  };
}

/**
 * The `FieldPolicySetRequest` of a form. The allowed fields are the known
 * ones left shown (or allowed or required), the typed-in ones and the unnamed
 * positions of the dictionary's segments that the rule does not keep out; a
 * field set to "Ukryj" is never listed as allowed.
 */
export function buildPolicyRequest({ instanceId, topic, subject, direction, form, source }) {
  const names = source.fields.map((f) => f.name);
  const eff = effectiveForm(form, source, direction);
  const read = direction === 'read';
  const allowedKnown = names.filter((n) => (read ? eff.actions[n] !== 'hide' : eff.actions[n] !== 'forbid'));
  const requiredKnown = read ? [] : names.filter((n) => eff.actions[n] === 'require');
  const kept = new Set(eff.hiddenImplicit);
  const implicit = source.implicit.filter((n) => !kept.has(n));
  return {
    instanceId,
    topic,
    subjectType: subject.subjectType,
    subjectId: subject.subjectId,
    direction,
    fields: sortedUnique([...allowedKnown, ...eff.extraShown, ...eff.extraRequired, ...implicit]),
    requiredFields: sortedUnique([...requiredKnown, ...eff.extraRequired]),
  };
}

/**
 * What a form still lacks to be a rule worth storing, in words, or `null`:
 * every typed-in address the format refuses (each one named, so none is
 * found only after the one before it was fixed), a typed name that
 * contradicts the row of the same field, or no field set to anything. A rule
 * that hides nothing in a COMPLETE list does nothing — the way to switch one
 * off is to delete it; over an incomplete list it still hides (or refuses)
 * what the list does not name, which the sentence about it says.
 */
export function formProblem({ direction, form, source, format }) {
  const typed = [...form.extraShown, ...form.extraRequired];
  const problems = [];
  for (const name of typed) {
    const reason = fieldNameProblem(format, name);
    if (reason) problems.push(T(`hiding.field_problem.${reason}`, { name, suggestion: hl7Suggestion(name) ?? '' }));
  }
  if (problems.length) return problems.join(' ');
  const known = new Set(source.fields.map((f) => f.name));
  const against = direction === 'read' ? 'hide' : 'forbid';
  for (const name of typed.map((n) => n.trim())) {
    if (known.has(name) && form.actions[name] === against) return T('hiding.field_problem.typed_known', { name });
  }
  const listed = source.fields.length > 0;
  const eff = effectiveForm(form, source, direction);
  if (direction === 'read') {
    if (listed) return source.complete && !source.fields.some((f) => eff.actions[f.name] === 'hide') ? T('hiding.need.read') : null;
    return eff.extraShown.length ? null : T('hiding.need.read_typed');
  }
  if (listed) {
    const set = source.fields.some((f) => eff.actions[f.name] === 'forbid' || eff.actions[f.name] === 'require');
    return set || eff.extraRequired.length || !source.complete ? null : T('hiding.need.write');
  }
  return eff.extraShown.length || eff.extraRequired.length ? null : T('hiding.need.write_typed');
}

const phrases = (source, names) => names.map((n) => fieldPhrase(source, n)).join(', ');
const minus = (list, other) => list.filter((n) => !other.includes(n));

/**
 * Whether saving a rule for chosen people, groups or addons closes the topic
 * to systems with an API key: the first rule of a direction that is not the
 * rule for everyone (`bus.key_needs_topic_wide_rule`). `rules` = the stored
 * rows.
 */
export function closesTopicToKeys({ rules, subjectType, direction }) {
  return subjectType !== 'any' && !rules.some((r) => r.direction === direction);
}

/**
 * The sentences of "Co się stanie po zapisaniu". `who` = the name the rule
 * goes by; `current` = the form of the rule as it is stored (`null` for a
 * new rule). Fields are named, never counted, so the sentences hold for every
 * list; what stays unchanged is said only when something else changes. They
 * describe what `buildPolicyRequest` sends, and say none of them where the
 * window is: they are also the text of the note after the window closed.
 */
export function ruleImpact({ who, direction, form, current, source }) {
  const names = source.fields.map((f) => f.name);
  const eff = effectiveForm(form, source, direction);
  const was = current ? effectiveForm(current, source, direction) : null;
  const lines = [];
  if (direction === 'read') {
    const away = (f) => names.filter((n) => f?.actions[n] === 'hide');
    const nowAway = away(eff);
    const wasAway = away(was);
    const hiddenMore = minus(nowAway, wasAway);
    const shownMore = minus(wasAway, nowAway);
    const typedMore = minus(eff.extraShown, was?.extraShown || []);
    const typedLess = minus(was?.extraShown || [], eff.extraShown);
    if (!names.length) return [T('hiding.impact.read_only', { who, fields: eff.extraShown.join(', ') })];
    if (hiddenMore.length || typedLess.length) lines.push(T('hiding.impact.read_hidden', { who, fields: phrases(source, [...hiddenMore, ...typedLess]) }));
    // A new rule hides; the fields it leaves shown were shown before it.
    if (current && (shownMore.length || typedMore.length)) lines.push(T('hiding.impact.read_shown', { who, fields: phrases(source, [...shownMore, ...typedMore]) }));
    if (lines.length) lines.push(T('hiding.impact.rest'), T('hiding.impact.read_unlisted'));
    else if (!current && !source.complete) lines.push(T('hiding.impact.read_unlisted_only', { who }));
    return lines;
  }
  const forbidden = (f) => names.filter((n) => f?.actions[n] === 'forbid');
  const required = (f) => sortedUnique([...names.filter((n) => f?.actions[n] === 'require'), ...(f?.extraRequired || [])]);
  const typedAllowed = (f) => sortedUnique([...(f?.extraShown || []), ...(f?.extraRequired || [])]);
  if (!names.length) {
    lines.push(T('hiding.impact.write_only', { who, fields: typedAllowed(eff).join(', ') }));
    if (required(eff).length) lines.push(T('hiding.impact.write_required', { who, fields: phrases(source, required(eff)) }));
    if (lines.length) lines.push(T('hiding.impact.write_rest'));
    return lines;
  }
  if (!was) {
    if (forbidden(eff).length) lines.push(T('hiding.impact.write_forbidden', { who, fields: phrases(source, forbidden(eff)) }));
    if (required(eff).length) lines.push(T('hiding.impact.write_required', { who, fields: phrases(source, required(eff)) }));
    if (lines.length) lines.push(T('hiding.impact.write_rest'), T('hiding.impact.write_unlisted'));
    else if (!source.complete) lines.push(T('hiding.impact.write_unlisted_only', { who }));
    return lines;
  }
  const refusedMore = [...minus(forbidden(eff), forbidden(was)), ...minus(typedAllowed(was), typedAllowed(eff))];
  const acceptedMore = [...minus(forbidden(was), forbidden(eff)), ...minus(typedAllowed(eff), typedAllowed(was))];
  const requiredMore = minus(required(eff), required(was));
  const requiredLess = minus(required(was), required(eff));
  if (refusedMore.length) lines.push(T('hiding.impact.write_forbidden', { who, fields: phrases(source, refusedMore) }));
  if (acceptedMore.length) lines.push(T('hiding.impact.write_accepted', { who, fields: phrases(source, acceptedMore) }));
  if (requiredMore.length) lines.push(T('hiding.impact.write_required', { who, fields: phrases(source, requiredMore) }));
  if (requiredLess.length) lines.push(T('hiding.impact.write_not_required', { who, fields: phrases(source, requiredLess) }));
  if (refusedMore.length || requiredMore.length) lines.push(T('hiding.impact.write_rest'));
  if (lines.length) lines.push(T('hiding.impact.write_unlisted'));
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

/** The legend under the table: what "Ukryj" and a refused message mean, and which rule wins. */
export function legendHtml() {
  const items = [
    `<span><b>${escapeHtml(T('hiding.action.hide'))}</b> — ${escapeHtml(T('hiding.legend.hide'))}</span>`,
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
 * (`name`, `contentType`), `access`,
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
      ? `<tf-empty-state badge icon="shield" title="${escapeAttr(T('hiding.blocked_title'))}" message="${escapeAttr(T('hiding.blocked_text'))}"><tf-button variant="secondary" icon="settings" data-go="section" data-section="settings">${escapeHtml(T('hiding.blocked_settings'))}</tf-button></tf-empty-state>`
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
  setAttr(preview, 'disabled', !view.access?.canRead || !loaded || blocked);
  setAttr(preview, 'title', !view.access?.canRead ? T('detail.preview_no_read', { name: view.topic.name }) : blocked ? T('hiding.blocked_preview') : null);

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
  patchHtml(host.querySelector('[data-role="legend"]'), legendHtml());
  setRowsIfChanged(table, rows.map((r) => ruleTableRow(r, source, view.nowMs ?? Date.now())));
}
