// ===== File: modules/tentabus/topic-hiding.js — a topic's Ukrywanie danych section: which fields each person, group or addon sees or may write =====
//
// The server stores a rule as the fields it ALLOWS (`BusFieldPolicyWire.fields`):
// one rule per subject and direction, a field that is not allowed is hidden on
// reading and refused on writing. The screen shows it the other way round —
// what is hidden — because that is what an administrator decides:
//   - reading: "Ukryj" takes a field out of the allowed list; the table lists
//     the known fields that are not allowed. "Zamaskuj" and "Zahaszuj" keep a
//     field in the allowed list and name it again in `masked_fields` /
//     `hashed_fields` (a subset of the allowed ones, never both): the reader
//     gets the last 3 characters behind asterisks, or a keyed hash of the value
//     shared by every instance of the organization. Only a reading rule can do
//     that, and only where the server offers it (`capabilities.fieldActions`);
//   - writing: each field is allowed, required (`required_fields`, a subset of
//     the allowed ones) or not allowed; a message with a field that is not
//     allowed, or without a required one, is refused as a whole.
//
// What the server does, which the windows say before anyone confirms:
//   - a person's own rule wins over the rules of their groups, which win over
//     the rule for everyone; rules are never added together
//     (`field_policies::resolve`);
//   - a field the window does not list is not allowed either, so it is hidden
//     (or refused): the allowed list is the only thing stored. The one
//     exception is a HL7 WRITING rule, which allows the unnamed positions of
//     the dictionary's segments (hl7-fields.js) so real messages are not
//     refused; a READING rule never does;
//   - a general API key meets only the rule for everyone, and a topic with
//     rules for chosen subjects but none for everyone is closed to keys.
//
// The known fields come from the topic's message pattern (a JSON Schema's
// properties, followed through allOf / oneOf / anyOf / if-then-else and local
// $ref) or, for HL7 v2, from a dictionary (hl7-fields.js) whose unnamed
// positions a writing rule allows by default; a topic with neither (XML, JSON without a
// pattern, a pattern the screen cannot read to the end) is listed by hand. The
// shell (tentabus.js) loads the rules and the pattern; this module paints the
// section and builds the requests, the windows are in topic-hiding-windows.js.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml, setAttr, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtDate, fmtDayTime, contentKind } from '/js/modules/tentabus/format.js';
import { schemaKind } from '/js/modules/tentabus/schemas.js';
import { showHidden } from '/js/modules/tentabus/hidden-characters.js';
import { sentenceHtml } from '/js/modules/tentabus/hidden-notice.js';
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
/** What a reading rule can do to a field besides showing it, widest first; the server says which it offers (`fieldActions`). */
const READ_HIDING_ACTIONS = ['hide', 'mask', 'hash'];

const KIND_ORDER = { any: 0, group: 1, user: 2, addon: 3 };
const listFormat = (items) => new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(items);

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/** `read` / `write` / `any` — how a rule is addressed in a row key and in a request. */
export const policyKey = (p) => `${p.subjectType}:${p.subjectId}:${p.direction}`;

/**
 * One row per rule (subject and direction): `{ key, subjectType, subjectId,
 * direction, label, memberCount, fields, requiredFields, maskedFields,
 * hashedFields, updatedAtMs }` (`fields` = every field the rule shows, as it
 * is, masked or hashed).
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
    maskedFields: [...(p.maskedFields || [])],
    hashedFields: [...(p.hashedFields || [])],
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

/**
 * What the payload of a topic is, as the server's field codecs see it: `json`,
 * `xml` or `hl7v2`. Anything else, `application/octet-stream` (the default
 * and the pre-F0 JSON topics) included, is JSON to the server. Whether rules
 * can work at all depends on the pattern bound to the topic, not on this:
 * see `fieldSource`.
 */
export function topicFormat(contentType) {
  const kind = contentKind(contentType);
  return kind === 'xml' || kind === 'hl7v2' ? kind : 'json';
}

/** How deep the walk follows a pattern before it gives up and calls it complex. */
const SCHEMA_DEPTH = 32;

const clip = (text, max = 70) => (text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text);

/**
 * Reads the fields of a JSON Schema: `{ fields: [{ name, label }], complex,
 * closed }`. The fields are the properties of the schema, its allOf / oneOf /
 * anyOf branches, if / then / else and every local `$ref` (`#/$defs/..`,
 * `#/definitions/..`), in the order the pattern writes them; the label is the
 * property's title, else the start of its description. `complex` is true when
 * the pattern names fields this walk cannot list — a remote or dangling
 * `$ref`, `patternProperties`, an `additionalProperties` schema — so the list
 * would be incomplete (also: `dependentSchemas`, an `unevaluatedProperties`
 * schema, a `$ref` that does not lead to a schema object, nesting deeper than
 * `SCHEMA_DEPTH`); `closed` when the schema forbids any other property. No
 * fields for text that is not a schema.
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
  const collect = (node, depth) => {
    if (!isObject(node) || seen.has(node)) return;
    if (depth > SCHEMA_DEPTH) {
      complex = true;
      return;
    }
    seen.add(node);
    if ('$ref' in node) {
      const target = resolve(node.$ref);
      // `true` / `false` as a schema says nothing about its fields.
      if (!isObject(target)) complex = true;
      else collect(target, depth + 1);
    }
    if (isObject(node.properties)) {
      for (const [name, def] of Object.entries(node.properties)) {
        if (found.has(name)) continue;
        const label = isObject(def) ? String(def.title || def.description || '').trim() : '';
        found.set(name, { name, label: clip(label) });
      }
    }
    if (isObject(node.patternProperties) && Object.keys(node.patternProperties).length) complex = true;
    if (isObject(node.additionalProperties) || isObject(node.unevaluatedProperties)) complex = true;
    if (isObject(node.dependentSchemas) && Object.keys(node.dependentSchemas).length) complex = true;
    for (const key of ['allOf', 'oneOf', 'anyOf']) {
      if (Array.isArray(node[key])) node[key].forEach((branch) => collect(branch, depth + 1));
    }
    for (const key of ['if', 'then', 'else']) collect(node[key], depth + 1);
  };
  collect(root, 0);
  return { fields: [...found.values()], complex, closed: isObject(root) && root.additionalProperties === false };
}

const localName = (name) => String(name ?? '').replace(/^.*:/, '');

/** The elements of an XML text as `{ name, attrs, children }` (local names, no text); `null` for text that is not a well-formed tree. */
function readXmlTree(text) {
  const source = String(text ?? '').replace(/<!--[\s\S]*?-->/g, '').replace(/<\?[\s\S]*?\?>/g, '').replace(/<!\[CDATA\[[\s\S]*?\]\]>/g, '').replace(/<!DOCTYPE[^>]*>/gi, '');
  const tag = /<(\/?)([^\s/>]+)((?:\s+[^\s=/>]+\s*=\s*(?:"[^"]*"|'[^']*'))*)\s*(\/?)>/g;
  const attr = /([^\s=]+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g;
  const stack = [];
  let root = null;
  let last = 0;
  for (let m = tag.exec(source); m; m = tag.exec(source)) {
    if (source.slice(last, m.index).includes('<')) return null;
    last = tag.lastIndex;
    const [, closing, rawName, rawAttrs, selfClosing] = m;
    if (closing) {
      const open = stack.pop();
      if (!open || open.raw !== rawName) return null;
      continue;
    }
    const attrs = {};
    for (let a = attr.exec(rawAttrs); a; a = attr.exec(rawAttrs)) attrs[a[1]] = a[2] ?? a[3] ?? '';
    const node = { raw: rawName, name: localName(rawName), attrs, children: [] };
    if (stack.length) stack[stack.length - 1].children.push(node);
    else if (root) return null;
    else root = node;
    if (!selfClosing) stack.push(node);
  }
  return stack.length || !root ? null : root;
}

/**
 * The fields of an XSD pattern: `{ fields: [{ name, label }], complex }`. The
 * server addresses an XML field by the local name of a DIRECT CHILD of the
 * root element, so these are the elements a root's content model lists
 * (sequence, choice and all, nested at will; an element named by `ref` counts
 * by its name). `complex` is true when that list cannot be written out — a
 * `group`, `any`, an extension of another type, a type that is not in the
 * file, or elements in a namespace (their prefix in a message is anybody's
 * guess, and a field is the literal name) — and the caller then asks for the
 * fields by hand. A pattern with several top-level elements lists the
 * children of all of them.
 */
export function readXsdFields(text) {
  const empty = { fields: [], complex: false };
  const schema = readXmlTree(text);
  if (!schema || schema.name !== 'schema') return empty;
  if (schema.attrs.targetNamespace && schema.attrs.elementFormDefault === 'qualified') return { fields: [], complex: true };
  const types = new Map(schema.children.filter((n) => n.name === 'complexType' && n.attrs.name).map((n) => [n.attrs.name, n]));
  const roots = schema.children.filter((n) => n.name === 'element' && n.attrs.name);
  const found = new Map();
  let complex = false;
  const walk = (node, depth) => {
    if (depth > SCHEMA_DEPTH) { complex = true; return; }
    for (const child of node.children) {
      if (child.name === 'element') {
        const name = child.attrs.name || localName(child.attrs.ref);
        if (name) found.set(name, { name, label: '' });
        else complex = true;
      } else if (child.name === 'sequence' || child.name === 'choice' || child.name === 'all') walk(child, depth + 1);
      else if (child.name === 'group' || child.name === 'any' || child.name === 'complexContent') complex = true;
    }
  };
  const contentOf = (element) => {
    const inline = element.children.find((n) => n.name === 'complexType');
    if (inline) return inline;
    const typeName = element.attrs.type;
    if (!typeName) return null;
    const named = types.get(localName(typeName));
    // A type of the XSD vocabulary (xs:string) has no children; any other must be in this file.
    if (!named && !/^(xs|xsd):/.test(typeName)) complex = true;
    return named ?? null;
  };
  for (const root of roots) {
    const content = contentOf(root);
    if (content) walk(content, 0);
  }
  return { fields: [...found.values()], complex };
}

/**
 * Where a topic's known fields come from. `schema` = `{ subject, version,
 * text }` of the pattern the topic checks with, `{ failed: true }` when it
 * could not be read, or `null`. Returns `{ mode, fields, complete, implicit,
 * subject, version, failed, complex }` with `mode`:
 *   - `schema`: the pattern's properties;
 *   - `dictionary`: HL7 v2's common fields (any other address can be typed),
 *     with `implicit` = the unnamed positions of the same segments, which a
 *     new WRITING rule allows without a row of their own (a reading rule hides
 *     them like any other field it does not list);
 *   - `manual`: no list — the fields to keep are typed in (`complex` = the
 *     pattern is there but names fields the screen cannot list);
 *   - `blocked`: the topic is checked with an Avro, Protobuf or Thrift pattern
 *     (`schema.schemaType`), whose bytes a rule cannot read: the server
 *     refuses rules there (`bus_field_policy_set`), so the screen does too.
 * `complete` says the list is every field a message can carry (a closed
 * pattern): only then can a rule that leaves every listed field alone be a
 * rule that does nothing.
 */
export function fieldSource({ format, schema }) {
  if (schemaKind(schema?.schemaType) === 'binary') return { mode: 'blocked', fields: [], implicit: [], complete: false };
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
  // An XML topic checked with an XSD pattern has fields, only they may not be listable: the screen then says so, not that there is no pattern.
  const xsd = format === 'xml' && schema?.schemaType === 'xsd' && !schema.failed;
  if (xsd) {
    const read = readXsdFields(schema.text);
    if (read.fields.length && !read.complex) {
      return { mode: 'schema', fields: read.fields, implicit: [], complete: false, subject: schema.subject, version: schema.version };
    }
  }
  return { mode: 'manual', fields: [], implicit: [], complete: false, failed: Boolean(schema?.failed), complex, xsd };
}

/** Field names in the order a person reads them: PID-3 before PID-11, `a2` before `a10`. */
export const compareFieldNames = (a, b) => String(a).localeCompare(String(b), undefined, { numeric: true, sensitivity: 'variant' });

/** The plain name of a field in `source`, '' when the source has none. */
export function fieldLabel(source, name) {
  return showHidden(source.fields.find((f) => f.name === name)?.label || hl7FieldLabel(name) || '');
}

/** "PESEL pacjenta (pacjent.pesel)", or the bare address when the field has no name. */
export function fieldPhrase(source, name) {
  const label = fieldLabel(source, name);
  return label ? `${label} (${showHidden(name)})` : showHidden(name);
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

/** Each typed-in address the format refuses, with the sentence that says why: `[{ name, text }]`. */
export function typedProblems(format, names) {
  const out = [];
  for (const name of names) {
    const reason = fieldNameProblem(format, name);
    if (reason) out.push({ name, text: T(`hiding.field_problem.${reason}`, { name, suggestion: hl7Suggestion(name) ?? '' }) });
  }
  return out;
}

// ---------------------------------------------------------------------------
// What a rule does, in words
// ---------------------------------------------------------------------------

/** The unnamed positions a rule of `direction` allows without a row: only a writing rule does. */
const implicitFor = (source, direction) => (direction === 'write' ? source.implicit : []);

/**
 * What a rule leaves out of the known fields: `{ known, hidden, marks,
 * forbidden, required, visible, plain, visibleOutside }`. `known` says whether
 * the source lists fields at all; without a list only the allowed fields are
 * known (`visible`, every field the rule shows in any form; `plain` = those
 * shown as they are). `marks` = `[name, 'hide' | 'mask' | 'hash']` of what a
 * READING rule does to a field: the listed ones in the order of the list, the
 * typed-in masked and hashed ones after them by name. `visibleOutside` = what a
 * READING rule lets through unchanged beyond the listed fields (typed in, or
 * positions a rule written elsewhere allows): they stay visible although the
 * window has no row for them.
 */
export function ruleFacts(row, source) {
  const listed = source.fields.map((f) => f.name);
  const names = [...listed, ...implicitFor(source, row.direction)];
  const allowed = new Set(row.fields);
  const outside = names.filter((n) => !allowed.has(n));
  const known = new Set(listed);
  const read = row.direction === 'read';
  const masked = new Set(read ? row.maskedFields : []);
  const hashed = new Set(read ? row.hashedFields : []);
  const changeOf = (n) => (hashed.has(n) ? 'hash' : masked.has(n) ? 'mask' : null);
  const plain = [...row.fields].filter((n) => !changeOf(n)).sort();
  const marks = [
    ...listed.map((n) => [n, allowed.has(n) ? changeOf(n) : 'hide']).filter(([, action]) => action),
    ...[...new Set([...masked, ...hashed])].filter((n) => !known.has(n)).sort().map((n) => [n, changeOf(n)]),
  ];
  return {
    known: listed.length > 0,
    hidden: read ? outside : [],
    marks: read ? marks : [],
    forbidden: read ? [] : outside,
    required: read ? [] : [...row.requiredFields].sort(),
    visible: [...row.fields].sort(),
    plain,
    visibleOutside: read && listed.length > 0 ? plain.filter((n) => !known.has(n)) : [],
  };
}

const CHIP_TONE = { hide: 'neutral', mask: 'info', hash: 'accent' };
const chipHtml = (tone, text) => `<span class="tf-chip tf-chip--outline ${tone}">${escapeHtml(text)}</span>`;

function fieldLineHtml(source, name, chip) {
  const label = fieldLabel(source, name);
  return `<div><span class="tf-table__cell--mono">${sentenceHtml(showHidden(name))}</span> ${chip}</div>${label ? `<div class="tf-table__cell-sub">${escapeHtml(label)}</div>` : ''}`;
}

function limitedHtml(source, items) {
  // "i jeszcze 1 pole" would take the room of the field it replaces: one more is named instead.
  const named = items.length === CELL_FIELDS + 1 ? items.length : CELL_FIELDS;
  const shown = items.slice(0, named).map(([name, chip]) => fieldLineHtml(source, name, chip)).join('');
  const rest = items.length - named;
  return rest > 0
    ? `${shown}<div class="tf-table__cell-sub">${escapeHtml(T('hiding.and_more', { count: fmtCount(rest), n: rest }))}</div>`
    : shown;
}

/** The "Co widzi inaczej" cell of a rule: the fields it hides (or refuses, or requires), one line each. */
export function ruleWhatHtml(row, source) {
  const facts = ruleFacts(row, source);
  const plain = (text) => `<span>${sentenceHtml(text)}</span>`;
  if (row.direction === 'read') {
    // A cell that has to cut its list cuts the plain "Ukryj" lines: what is masked or hashed is what the reader of the table looks for.
    const ordered = [...facts.marks.filter(([, action]) => action !== 'hide'), ...facts.marks.filter(([, action]) => action === 'hide')];
    const marked = ordered.map(([n, action]) => [n, chipHtml(CHIP_TONE[action], T(`hiding.action.${action}`))]);
    if (!facts.known) {
      const fields = facts.plain.map(showHidden).join(', ');
      let head = plain(T('hiding.sees_nothing'));
      if (facts.plain.length) head = plain(T(marked.length ? 'hiding.sees_only_and' : 'hiding.sees_only', { fields }));
      else if (marked.length) head = plain(T('hiding.sees_only_below'));
      return `${head}${marked.length ? limitedHtml(source, marked) : ''}`;
    }
    const hiddenHtml = marked.length ? limitedHtml(source, marked) : plain(T('hiding.hides_none'));
    // Over an incomplete list a reading rule hides every position it does not allow too.
    const hiddenRest = source.complete ? '' : `<div class="tf-table__cell-sub">${escapeHtml(T('hiding.reads_rest'))}</div>`;
    if (!facts.visibleOutside.length) return `${hiddenHtml}${hiddenRest}`;
    const shown = facts.visibleOutside.slice(0, CELL_FIELDS).map(showHidden).join(', ');
    const rest = facts.visibleOutside.length - CELL_FIELDS;
    const outside = T('hiding.visible_outside', { fields: rest > 0 ? `${shown} ${T('hiding.and_more', { count: fmtCount(rest), n: rest })}` : shown });
    return `${hiddenHtml}<div class="tf-table__cell-sub">${sentenceHtml(outside)}</div>${hiddenRest}`;
  }
  const requiredItems = facts.required.map((n) => [n, chipHtml('ok', T('hiding.write.require'))]);
  const refused = `<div class="tf-table__cell-sub">${escapeHtml(T('hiding.writes_rest'))}</div>`;
  if (!facts.known) {
    const only = plain(facts.visible.length ? T('hiding.writes_only', { fields: facts.visible.map(showHidden).join(', ') }) : T('hiding.writes_nothing'));
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

/** What a reading rule can do to a field: "Pokaż", "Ukryj" always, and "Zamaskuj" / "Zahaszuj" where `fieldActions` (the server's capabilities) lists them. */
export function readActionList(fieldActions) {
  const offered = new Set(fieldActions || []);
  return ['show', ...READ_HIDING_ACTIONS.filter((a) => a === 'hide' || offered.has(a))];
}

/** The segmented options of one field in a window: reading offers "Pokaż" and the hiding actions, writing allowed / required / not allowed. */
export function actionOptions(direction, fieldActions) {
  const variants = { show: 'ok', hide: 'warn', mask: 'info', hash: 'accent' };
  if (direction === 'read') {
    return readActionList(fieldActions).map((value) => ({ value, label: T(`hiding.action.${value}`), variant: variants[value] }));
  }
  return [['allow', 'ok'], ['require', 'accent'], ['forbid', 'err']].map(([value, variant]) => ({ value, label: T(`hiding.write.${value}`), variant }));
}

// ---------------------------------------------------------------------------
// A rule as the window edits it, and the request that stores it
// ---------------------------------------------------------------------------

/**
 * The action of each listed field by name. A field may be called anything a
 * JSON Schema allows — `__proto__` included — so the map has no prototype:
 * assigning `actions['__proto__']` on a plain object changes its prototype and
 * stores nothing.
 */
const actionMap = (entries = []) => Object.assign(Object.create(null), Object.fromEntries(entries));

const defaultAction = (direction) => (direction === 'read' ? 'show' : 'allow');
const sortedUnique = (list) => [...new Set(list)].sort();

/**
 * The form of a new rule: every known field shown (reading) or allowed
 * (writing), nothing typed in, no unnamed position kept out. `hiddenImplicit`
 * holds the unnamed positions a stored rule leaves out: the window has no row
 * for them, but saving the rule again must not allow them.
 */
export function blankForm(direction, source) {
  return {
    actions: actionMap(source.fields.map((f) => [f.name, defaultAction(direction)])),
    extraShown: [],
    extraMasked: [],
    extraHashed: [],
    extraRequired: [],
    hiddenImplicit: [],
  };
}

/**
 * The form of a stored rule. A known field not among the allowed ones is
 * "Ukryj" (or "Niedozwolone"), one the rule masks or hashes is "Zamaskuj" or
 * "Zahaszuj"; the allowed ones outside the known list are the typed-in fields
 * (each in the list of what happens to it). An unnamed position of the dictionary's segments is
 * not typed in on a WRITING rule (it is allowed by default); on a READING rule
 * it is, because a reading rule that lets one through is showing it, and the
 * administrator must see it to take it out.
 */
export function formFromRule(row, source) {
  const names = source.fields.map((f) => f.name);
  const known = new Set(names);
  const write = row.direction === 'write';
  const implicit = new Set(implicitFor(source, row.direction));
  const allowed = new Set(row.fields);
  const required = new Set(row.requiredFields);
  const masked = new Set(write ? [] : row.maskedFields);
  const hashed = new Set(write ? [] : row.hashedFields);
  const actions = actionMap();
  for (const n of names) {
    if (row.direction === 'read') actions[n] = !allowed.has(n) ? 'hide' : hashed.has(n) ? 'hash' : masked.has(n) ? 'mask' : 'show';
    else actions[n] = required.has(n) ? 'require' : allowed.has(n) ? 'allow' : 'forbid';
  }
  const typed = (n) => !known.has(n) && !implicit.has(n);
  return {
    actions,
    extraShown: sortedUnique(row.fields.filter((n) => typed(n) && !masked.has(n) && !hashed.has(n) && !(write && required.has(n)))),
    extraMasked: sortedUnique([...masked].filter((n) => typed(n) && !hashed.has(n))),
    extraHashed: sortedUnique([...hashed].filter(typed)),
    extraRequired: row.direction === 'write' ? sortedUnique(row.requiredFields.filter((n) => !known.has(n))) : [],
    hiddenImplicit: implicitFor(source, row.direction).filter((n) => !allowed.has(n)),
  };
}

/** A form as one string, so a window can tell whether anything changed. */
export function serializeForm(form, source) {
  return JSON.stringify([
    source.fields.map((f) => form.actions[f.name] ?? ''),
    sortedUnique(form.extraShown),
    sortedUnique(form.extraMasked || []),
    sortedUnique(form.extraHashed || []),
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
  if (!Array.isArray(parsed) || parsed.length !== 6) return null;
  const [actions, extraShown, extraMasked, extraHashed, extraRequired, hiddenImplicit] = parsed;
  return {
    actions: actionMap(source.fields.map((f, i) => [f.name, actions[i] ?? ''])),
    extraShown: [...extraShown],
    extraMasked: [...extraMasked],
    extraHashed: [...extraHashed],
    extraRequired: [...extraRequired],
    hiddenImplicit: [...hiddenImplicit],
  };
}

/**
 * The form as the request will carry it: the typed-in names trimmed, a typed
 * "required" name that is also a listed field makes that row "Wymagane" (a
 * typed masked or hashed one makes a shown row "Zamaskuj" or "Zahaszuj"), and
 * what stays typed is only what the list does not have. One place decides
 * this, so the request and the sentences about it cannot disagree. A writing
 * rule has no masked or hashed fields.
 */
function effectiveForm(form, source, direction) {
  const known = new Set(source.fields.map((f) => f.name));
  const read = direction === 'read';
  const typed = (list) => sortedUnique((list || []).map((n) => n.trim()).filter(Boolean));
  const required = read ? [] : typed(form.extraRequired);
  const masked = read ? typed(form.extraMasked) : [];
  const hashed = read ? typed(form.extraHashed) : [];
  const actions = Object.assign(Object.create(null), form.actions);
  for (const n of required) if (known.has(n) && actions[n] !== 'forbid') actions[n] = 'require';
  for (const [names, action] of [[masked, 'mask'], [hashed, 'hash']]) {
    for (const n of names) if (known.has(n) && actions[n] === 'show') actions[n] = action;
  }
  return {
    actions,
    extraShown: typed(form.extraShown).filter((n) => !known.has(n)),
    extraMasked: masked.filter((n) => !known.has(n)),
    extraHashed: hashed.filter((n) => !known.has(n)),
    extraRequired: required.filter((n) => !known.has(n)),
    hiddenImplicit: form.hiddenImplicit || [],
  };
}

/**
 * The `FieldPolicySetRequest` of a form. The allowed fields are the known
 * ones left shown (or allowed or required, masked or hashed), the typed-in ones
 * and, for a WRITING rule only, the unnamed positions of the dictionary's
 * segments that the rule does not keep out; a field set to "Ukryj" is never
 * listed as allowed. A masked or hashed field is allowed AND named in
 * `maskedFields` / `hashedFields`, which only a reading rule carries.
 */
export function buildPolicyRequest({ instanceId, topic, subject, direction, form, source }) {
  const names = source.fields.map((f) => f.name);
  const eff = effectiveForm(form, source, direction);
  const read = direction === 'read';
  const allowedKnown = names.filter((n) => (read ? eff.actions[n] !== 'hide' : eff.actions[n] !== 'forbid'));
  const requiredKnown = read ? [] : names.filter((n) => eff.actions[n] === 'require');
  const maskedKnown = read ? names.filter((n) => eff.actions[n] === 'mask') : [];
  const hashedKnown = read ? names.filter((n) => eff.actions[n] === 'hash') : [];
  const kept = new Set(eff.hiddenImplicit);
  const implicit = implicitFor(source, direction).filter((n) => !kept.has(n));
  return {
    instanceId,
    topic,
    subjectType: subject.subjectType,
    subjectId: subject.subjectId,
    direction,
    fields: sortedUnique([...allowedKnown, ...eff.extraShown, ...eff.extraMasked, ...eff.extraHashed, ...eff.extraRequired, ...implicit]),
    requiredFields: sortedUnique([...requiredKnown, ...eff.extraRequired]),
    maskedFields: sortedUnique([...maskedKnown, ...eff.extraMasked]),
    hashedFields: sortedUnique([...hashedKnown, ...eff.extraHashed]),
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
  const read = direction === 'read';
  const typedLists = read
    ? [['show', form.extraShown], ['mask', form.extraMasked || []], ['hash', form.extraHashed || []]]
    : [['allow', form.extraShown], ['allow', form.extraRequired]];
  const typed = typedLists.flatMap(([, list]) => list);
  const problems = typedProblems(format, typed);
  if (problems.length) return problems.map((p) => p.text).join(' ');
  const known = new Set(source.fields.map((f) => f.name));
  const seen = new Set();
  // Every doubled name at once, so none is found only after the one before it was fixed.
  const doubled = [];
  for (const name of typed.map((n) => n.trim())) {
    if (read && seen.has(name) && !doubled.includes(name)) doubled.push(name);
    seen.add(name);
  }
  if (doubled.length) return doubled.map((name) => T('hiding.field_problem.typed_twice', { name })).join(' ');
  // A typed name that is also a listed field may only say what its row already says (a shown row may become masked or hashed).
  const contradicts = (action, listAction) => {
    if (read) return action !== listAction && !(action === 'show' && listAction !== 'show');
    return action === 'forbid';
  };
  for (const [listAction, list] of typedLists) {
    for (const name of list.map((n) => n.trim())) {
      if (known.has(name) && form.actions[name] && contradicts(form.actions[name], listAction)) {
        return T('hiding.field_problem.typed_known', { name });
      }
    }
  }
  const listed = source.fields.length > 0;
  const eff = effectiveForm(form, source, direction);
  if (read) {
    const typedKept = eff.extraShown.length || eff.extraMasked.length || eff.extraHashed.length;
    if (listed) return source.complete && !typedKept && !source.fields.some((f) => eff.actions[f.name] !== 'show') ? T('hiding.need.read') : null;
    return typedKept ? null : T('hiding.need.read_typed');
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
 * goes by (`everyone` = it is the rule for everyone, which has no name to
 * quote); `current` = the form of the rule as it is stored (`null` for a new
 * rule). Fields are named, never counted, so the sentences hold for every
 * list; what stays unchanged is said only when something else changes. They
 * describe what `buildPolicyRequest` sends. With `saved` they are the text of
 * the note after the window closed, where there is nobody to tell "unless you
 * add them below".
 */
export function ruleImpact({ who, everyone = false, saved = false, direction, form, current, source }) {
  const names = source.fields.map((f) => f.name);
  const eff = effectiveForm(form, source, direction);
  const was = current ? effectiveForm(current, source, direction) : null;
  const named = T('hiding.who.named', { name: who });
  const whoFor = everyone ? T('hiding.who.everyone_for') : named;
  const whoFrom = everyone ? T('hiding.who.everyone_from') : named;
  const t = (key, params = {}) => T(key, { who: whoFor, whoFrom, ...params });
  // A pattern that lists every field a message can carry has no "other" fields to speak of.
  const unlisted = (key) => (source.complete ? [] : [t(`hiding.impact.${key}${saved ? '_saved' : ''}`)]);
  const hl7 = source.implicit.length > 0;
  const lines = [];
  if (direction === 'read') {
    // What each field becomes: its row for a listed one, the list it was typed into for another (typed in no list = hidden).
    const typedOf = (f) => Object.assign(Object.create(null),
      Object.fromEntries([...f.extraShown.map((n) => [n, 'show']), ...f.extraMasked.map((n) => [n, 'mask']), ...f.extraHashed.map((n) => [n, 'hash'])]));
    const nowTyped = typedOf(eff);
    const wasTyped = was ? typedOf(was) : null;
    const after = (n) => (names.includes(n) ? eff.actions[n] : nowTyped[n] ?? 'hide');
    const before = (n) => (!was ? 'show' : names.includes(n) ? was.actions[n] : wasTyped[n] ?? 'hide');
    const candidates = [...names, ...new Set([...Object.keys(nowTyped), ...Object.keys(wasTyped || {})])];
    const became = (action) => candidates.filter((n) => after(n) === action && before(n) !== action);
    const typedSeen = [...Object.keys(nowTyped)].sort();
    if (!names.length) {
      lines.push(t('hiding.impact.read_only', { fields: typedSeen.map(showHidden).join(', ') }));
      for (const action of ['mask', 'hash']) {
        const named = typedSeen.filter((n) => nowTyped[n] === action);
        if (named.length) lines.push(t(`hiding.impact.read_${action === 'mask' ? 'masked' : 'hashed'}`, { fields: named.map(showHidden).join(', '), n: named.length }));
      }
      return lines;
    }
    const hiddenMore = became('hide');
    const maskedMore = became('mask');
    const hashedMore = became('hash');
    if (hiddenMore.length) lines.push(t('hiding.impact.read_hidden', { fields: phrases(source, hiddenMore) }));
    if (maskedMore.length) lines.push(t('hiding.impact.read_masked', { fields: phrases(source, maskedMore), n: maskedMore.length }));
    if (hashedMore.length) lines.push(t('hiding.impact.read_hashed', { fields: phrases(source, hashedMore), n: hashedMore.length }));
    // A new rule changes what it names; the fields it leaves shown were shown before it.
    const shownMore = current ? became('show') : [];
    if (shownMore.length) lines.push(t('hiding.impact.read_shown', { fields: phrases(source, shownMore) }));
    if (lines.length) lines.push(T('hiding.impact.rest'), ...unlisted('read_unlisted'));
    else if (!current && !source.complete) lines.push(t('hiding.impact.read_unlisted_only'));
    return lines;
  }
  const forbidden = (f) => names.filter((n) => f?.actions[n] === 'forbid');
  const required = (f) => sortedUnique([...names.filter((n) => f?.actions[n] === 'require'), ...(f?.extraRequired || [])]);
  const typedAllowed = (f) => sortedUnique([...(f?.extraShown || []), ...(f?.extraRequired || [])]);
  if (!names.length) {
    lines.push(t('hiding.impact.write_only', { fields: typedAllowed(eff).map(showHidden).join(', ') }));
    if (required(eff).length) lines.push(t('hiding.impact.write_required', { fields: phrases(source, required(eff)) }));
    if (lines.length) lines.push(T('hiding.impact.write_rest'));
    return lines;
  }
  const unlistedKey = hl7 ? 'write_unlisted_hl7' : 'write_unlisted';
  if (!was) {
    if (forbidden(eff).length) lines.push(t('hiding.impact.write_forbidden', { fields: phrases(source, forbidden(eff)) }));
    if (required(eff).length) lines.push(t('hiding.impact.write_required', { fields: phrases(source, required(eff)) }));
    if (lines.length) lines.push(T('hiding.impact.write_rest'), ...unlisted(unlistedKey));
    else if (!source.complete) lines.push(t(`hiding.impact.write_unlisted_only${hl7 ? '_hl7' : ''}`));
    return lines;
  }
  const refusedMore = [...minus(forbidden(eff), forbidden(was)), ...minus(typedAllowed(was), typedAllowed(eff))];
  const acceptedMore = [...minus(forbidden(was), forbidden(eff)), ...minus(typedAllowed(eff), typedAllowed(was))];
  const requiredMore = minus(required(eff), required(was));
  const requiredLess = minus(required(was), required(eff));
  if (refusedMore.length) lines.push(t('hiding.impact.write_forbidden', { fields: phrases(source, refusedMore) }));
  if (acceptedMore.length) lines.push(t('hiding.impact.write_accepted', { fields: phrases(source, acceptedMore) }));
  if (requiredMore.length) lines.push(t('hiding.impact.write_required', { fields: phrases(source, requiredMore) }));
  if (requiredLess.length) lines.push(t('hiding.impact.write_not_required', { fields: phrases(source, requiredLess) }));
  if (refusedMore.length || requiredMore.length) lines.push(T('hiding.impact.write_rest'));
  if (lines.length) lines.push(...unlisted(unlistedKey));
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
      <div data-role="blocked-note"></div>
      <tf-table data-role="rules">
        <tf-column key="who" label="${escapeAttr(T('hiding.col_who'))}" renderer="html"></tf-column>
        <tf-column key="what" label="${escapeAttr(T('hiding.col_what'))}" renderer="html" fill></tf-column>
        <tf-column key="when" label="${escapeAttr(T('hiding.col_when'))}" renderer="html"></tf-column>
        <tf-column key="changed" label="${escapeAttr(T('hiding.col_changed'))}" renderer="html"></tf-column>
      </tf-table>
      <div class="tb-table-footer" data-role="legend"></div>
    </div>
    <div class="section-card" data-role="key-card">
      <div class="section-card-head">
        <div class="title">${sprite('key')} ${escapeHtml(T('hiding.key.title'))}</div>
        <div class="actions">
          <tf-button variant="secondary" size="sm" icon="key" data-go="hiding-key" data-role="key-button">${escapeHtml(T('hiding.key.button'))}</tf-button>
        </div>
      </div>
      <div class="section-sub" data-role="key-text"></div>
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

/**
 * The legend under the table: what "Ukryj", "Zamaskuj" and "Zahaszuj" (the
 * last two only where `fieldActions` lists them) and a refused message mean,
 * and which rule wins.
 */
export function legendHtml(fieldActions) {
  const items = readActionList(fieldActions)
    .filter((action) => action !== 'show')
    .map((action) => `<span><b>${escapeHtml(T(`hiding.action.${action}`))}</b> — ${escapeHtml(T(`hiding.legend.${action}`, { example: T('hiding.legend.mask_example') }))}</span>`);
  items.push(
    `<span><b>${escapeHtml(T('hiding.legend.reject_name'))}</b> — ${escapeHtml(T('hiding.legend.reject'))}</span>`,
    `<span>${escapeHtml(T('hiding.legend.precedence'))}</span>`,
  );
  return items.join('');
}

/**
 * The text of "Klucz haszowania": what the key is, how far it reaches, and
 * - for the instance administrator, the only one who can read it - when it was
 * set. `key` = `{ info, error }` (`info` = `BusFieldHashKeyWire`), `null`
 * while it loads or for a reader who may not ask.
 */
export function keyNote({ canAdmin, orgName, key }) {
  const lines = [T(orgName ? 'hiding.key.explain' : 'hiding.key.explain_org', { org: orgName ?? '' })];
  const info = key?.info;
  if (!canAdmin) lines.push(T('hiding.key.admin_only'));
  else if (info?.exists) lines.push(T('hiding.key.current', { date: fmtDate(info.rotatedAtMs || info.createdAtMs), version: info.keyVersion }));
  else if (info) lines.push(T(keyAwaited(info) ? 'hiding.key.none_rules' : 'hiding.key.none'));
  else if (key?.error) lines.push(T('hiding.key.load_failed', { error: key.error }));
  // Only an operator node can hand a new key to the others (`canRotateHere`; an older server does not say, and is taken to allow it).
  if (canAdmin && info?.exists && info.canRotateHere === false) lines.push(T('hiding.key.not_operator'));
  return lines;
}

/** Whether this node has rules that hash but no key yet: the key is on its way from an operator node, and what the rules hash is hidden meanwhile. */
export function keyAwaited(info) {
  return info != null && info.exists === false && Number(info.hashingRules) > 0;
}

/** Why the key button cannot be used, in words, or `null` when it can. */
export function keyButtonProblem(info) {
  if (!info) return null;
  if (!info.exists) return T(keyAwaited(info) ? 'hiding.key.none_rules_title' : 'hiding.key.none_title');
  return info.canRotateHere === false ? T('hiding.key.not_operator') : null;
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
 * Paints "Klucz haszowania": the card is for topics that can have rules, its
 * button only for the instance administrator (`capabilities.canAdmin`) and
 * only once a key exists - it is made with the first "Zahaszuj" rule of the
 * organization, so there is nothing to change before.
 */
function paintKeyCard(host, view, data, blocked) {
  const card = host.querySelector('[data-role="key-card"]');
  card.hidden = blocked;
  const canAdmin = view.capabilities?.canAdmin === true;
  const lines = keyNote({ canAdmin, orgName: view.capabilities?.orgName || null, key: data.hashKey ?? null });
  patchHtml(card.querySelector('[data-role="key-text"]'), lines.map((l) => `<div>${escapeHtml(l)}</div>`).join(''));
  const info = data.hashKey?.info ?? null;
  const button = card.querySelector('[data-role="key-button"]');
  button.hidden = !canAdmin;
  const problem = canAdmin ? keyButtonProblem(info) : null;
  setAttr(button, 'disabled', !info || problem != null);
  setAttr(button, 'title', problem);
}

/**
 * Paints the Ukrywanie danych section. `view` carries the page's `topic`
 * (`name`, `contentType`), `access`, `capabilities` (`fieldActions`,
 * `canAdmin`, `orgName`),
 * `notice`, `nowMs` and `hidingData` = `{ policies, policiesError, schema,
 * schemaSettled, hashKey }` (`policies` null while loading; `schema` as in
 * `fieldSource`; `schemaSettled` false while the pattern is being read;
 * `hashKey` = `{ info, error }` of the organization's key, asked for the
 * instance administrator only).
 * Buttons call `ctx.go({ kind: 'hiding-add' | 'hiding-preview' |
 * 'hiding-key' | 'hiding-reload' })` and `{ kind: 'hiding-change' |
 * 'hiding-remove', rule }`.
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

  // Rules that an older topic already has, on content they cannot read: said plainly, and only "Usuń" is left.
  patchHtml(host.querySelector('[data-role="blocked-note"]'), blocked && rows.length
    ? `<tf-alert tone="warning" title="${escapeAttr(T('hiding.blocked_title'))}" message="${escapeAttr(T('hiding.blocked_with_rules'))}"></tf-alert>`
    : '');
  host.querySelector('[data-role="rules"]').__tbBlocked = blocked;
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
      const pairs = [['remove', T('hiding.remove.button'), () => ctx.go({ kind: 'hiding-remove', rule: live()._key })]];
      // A rule on binary content cannot be changed into anything that works: it can only go.
      if (!table.__tbBlocked) pairs.unshift(['change', T('hiding.change.button'), () => ctx.go({ kind: 'hiding-change', rule: live()._key })]);
      return rowButtons(pairs);
    };
  }
  patchHtml(host.querySelector('[data-role="legend"]'), legendHtml(view.capabilities?.fieldActions));
  paintKeyCard(host, view, data, blocked);
  setRowsIfChanged(table, rows.map((r) => ruleTableRow(r, source, view.nowMs ?? Date.now())));
}
