// ===== File: modules/tentabus/topic-access.js — a topic's Dostęp section: who may read, write and administer it, and the keys of outside systems =====
//
// The section (T09) has two cards. "Osoby, grupy i addony" lists the topic's
// own entries (`AclList`, one row per subject) as Czytanie / Zapis /
// Administracja: "pozwolono", "zabroniono" or "—" (not set here: the role in
// the organisation decides). "Systemy zewnętrzne (klucze API)" lists the
// general keys with a right to this topic's messages (the topic's `api_key`
// entries) or to the instance's message patterns (`bus_schema_registry`
// scopes of the key).
//
// What the server does, which the windows say before anyone confirms:
//   - an entry is kept per subject AND right (migration 168), so "Nadaj" and
//     "Zmień" send one `AclSet` per right that changes and "Usuń" clears every
//     right of the subject; an old entry written for every right at once
//     (`action = '*'`) is cleared first and the rights set one by one;
//   - at one level "zabroniono" beats "pozwolono", and a person's own entry
//     beats their groups' (`resource_permissions::check_action`);
//   - a 'user' entry naming an addon is refused on save (`bus.subject_is_addon`)
//     but can be removed: it is shown as an unknown subject with its id;
//   - a change is sent as several requests; they are ordered so that stopping
//     after any of them never leaves more access than before or after it:
//     new bans first, then allows taken away, then new allows, bans lifted last;
//   - revoking a key removes its rights with it; an entry of a key that no
//     longer exists (older data) grants nothing and is shown as such, with
//     "Usuń prawa" for the server administrator;
//   - keys are issued, changed and revoked by the server administrator only
//     (`ApiKeyCreate`/`ScopeSet`/`ScopeClear`/`Revoke` are site-admin and a
//     topic right also needs the topic's administration); a key's secret is
//     shown once; a key reads over REST under its own consumer groups
//     `k:<key id>` or `k:<key id>.<name>`.
//
// The shell (tentabus.js) loads the data and reloads it after every change;
// this module paints the section and opens the windows.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml, setAttr, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtDayTime } from '/js/modules/tentabus/format.js';
import { busSchemaScopeId, topicScopeId, BUS_SCHEMA_REGISTRY } from '/js/modules/access-keys-scopes.js';
import { windowEl, openChangeWindow, openConfirmWindow } from '/js/modules/tentabus/windows.js';
import '/js/components/tf-button.js';
import '/js/components/tf-table.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-select.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-input.js';
import '/js/components/tf-checkbox.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/** The three rights of a topic entry, in the order of the table. */
export const ACL_ACTIONS = ['read', 'write', 'admin'];
/** Who can be given an entry through "Nadaj dostęp" (the directory's kinds). */
export const SUBJECT_KINDS = ['group', 'user', 'addon'];
/** The four rights a key can carry here: two to the topic's messages, two to the instance's patterns. */
export const KEY_RIGHTS = ['readMessages', 'writeMessages', 'readSchemas', 'writeSchemas'];
/** `ApiKeyCreate` takes a name of 1–200 characters. */
export const KEY_NAME_MAX = 200;
/** Within this long of now a key's last use reads "przed chwilą". */
const JUST_NOW_MS = 60_000;

const KIND_ORDER = { group: 0, user: 1, addon: 2 };
const listFormat = (items) => new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(items);

// ---------------------------------------------------------------------------
// The subjects' table
// ---------------------------------------------------------------------------

/**
 * One row per subject of the topic's entries (keys excluded — they have their
 * own card): `{ subjectType, subjectId, label, memberCount, rights: { read,
 * write, admin }, wildcard }` where a right is 'allow' | 'deny' | null and
 * `wildcard` is the level of an old every-right entry, if any. Groups first,
 * then people, then addons, each by name.
 */
export function subjectRows(entries) {
  const bySubject = new Map();
  for (const e of entries || []) {
    const type = e.subjectType;
    if (type === 'api_key') continue;
    const key = `${type}:${e.subjectId}`;
    let row = bySubject.get(key);
    if (!row) {
      row = { subjectType: type, subjectId: e.subjectId, label: e.subjectLabel || null, memberCount: e.memberCount ?? null, rights: { read: null, write: null, admin: null }, wildcard: null };
      bySubject.set(key, row);
    }
    const level = e.accessLevel === 'deny' ? 'deny' : 'allow';
    if (e.action === '*') row.wildcard = level;
    else if (ACL_ACTIONS.includes(e.action)) row.rights[e.action] = level;
  }
  return [...bySubject.values()].sort((a, b) => (KIND_ORDER[a.subjectType] ?? 3) - (KIND_ORDER[b.subjectType] ?? 3)
    || Number(!a.label) - Number(!b.label)
    || String(a.label ?? a.subjectId).localeCompare(String(b.label ?? b.subjectId), I18n.getLanguage()));
}

/**
 * What one right of a row amounts to: a "zabroniono" anywhere wins, then a
 * "pozwolono" — the server sums an old every-right entry with the right's own.
 */
export function rightOf(row, action) {
  const own = row.rights[action];
  if (own === 'deny' || row.wildcard === 'deny') return 'deny';
  if (own === 'allow' || row.wildcard === 'allow') return 'allow';
  return null;
}

/** The three rights as they are now: `{ read, write, admin }`, each 'allow' | 'deny' | ''. */
export function currentRights(row) {
  return Object.fromEntries(ACL_ACTIONS.map((a) => [a, rightOf(row, a) || '']));
}

/** The name a row goes by; an id the node does not know is said to be unknown. */
export function subjectTitle(row) {
  return row.label || T('access.unknown_subject');
}

/** The second line of a row: the kind, a group's size, the id of an unknown subject. */
export function subjectSub(row) {
  const kind = T(`access.kind.${row.subjectType}`);
  if (!row.label) return T('access.unknown_sub', { kind, id: row.subjectId });
  if (row.subjectType === 'group' && row.memberCount != null) {
    return T('access.group_sub', { kind, count: fmtCount(row.memberCount), n: Number(row.memberCount) });
  }
  return kind;
}

function rightCellHtml(value) {
  if (value === 'allow') return `<span class="tf-chip tf-chip--outline ok">${escapeHtml(T('access.allowed'))}</span>`;
  if (value === 'deny') return `<span class="tf-chip tf-chip--outline err">${escapeHtml(T('access.denied'))}</span>`;
  return `<span title="${escapeAttr(T('access.not_set_title'))}">—</span>`;
}

/** A table row of the subjects' card. */
export function subjectTableRow(row) {
  return {
    who: `<span class="tf-table__cell-title">${escapeHtml(subjectTitle(row))}</span><div class="tf-table__cell-sub">${escapeHtml(subjectSub(row))}</div>`,
    read: rightCellHtml(rightOf(row, 'read')),
    write: rightCellHtml(rightOf(row, 'write')),
    admin: rightCellHtml(rightOf(row, 'admin')),
    _key: `${row.subjectType}:${row.subjectId}`,
  };
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/**
 * Orders requests so that stopping after any of them never leaves more
 * access than there was before or will be after: new bans first, then
 * allows taken away, then new allows, and bans lifted last. `kind` of each
 * step: 'deny' | 'clear-allow' | 'allow' | 'clear-deny'.
 */
const STEP_ORDER = ['deny', 'clear-allow', 'allow', 'clear-deny'];
function ordered(steps) {
  return STEP_ORDER.flatMap((kind) => steps.filter((s) => s.kind === kind).map((s) => s.request));
}

/**
 * The `AclSet` requests that turn `row` (null for a subject without an entry)
 * into `next` = `{ read, write, admin }` ('allow' | 'deny' | ''). An old
 * every-right entry is replaced by one entry per right; a right that goes to
 * '' is cleared only where its own entry exists. See `ordered` for the order.
 */
export function buildAclRequests(instanceId, topic, subject, row, next) {
  const base = { instanceId, topic, subjectType: subject.subjectType, subjectId: subject.subjectId };
  const steps = [];
  const own = row?.rights || { read: null, write: null, admin: null };
  if (row?.wildcard) {
    steps.push({ kind: row.wildcard === 'deny' ? 'clear-deny' : 'clear-allow', request: { ...base, accessLevel: 'clear', action: '*' } });
  }
  for (const action of ACL_ACTIONS) {
    const want = next[action] || null;
    const had = own[action];
    if (want && want !== had) steps.push({ kind: want, request: { ...base, accessLevel: want, action } });
    else if (!want && had) steps.push({ kind: had === 'deny' ? 'clear-deny' : 'clear-allow', request: { ...base, accessLevel: 'clear', action } });
  }
  return ordered(steps);
}

/** The `AclSet` requests that take every entry of `row` away: allows first, bans last. */
export function buildAclRemoveRequests(instanceId, topic, row) {
  const base = { instanceId, topic, subjectType: row.subjectType, subjectId: row.subjectId };
  const steps = ACL_ACTIONS.filter((a) => row.rights[a])
    .map((action) => ({ kind: row.rights[action] === 'deny' ? 'clear-deny' : 'clear-allow', request: { ...base, accessLevel: 'clear', action } }));
  if (row.wildcard) steps.push({ kind: row.wildcard === 'deny' ? 'clear-deny' : 'clear-allow', request: { ...base, accessLevel: 'clear', action: '*' } });
  return ordered(steps);
}

// ---------------------------------------------------------------------------
// "Co się stanie" of the subject windows
// ---------------------------------------------------------------------------

const actionsOf = (next, level) => ACL_ACTIONS.filter((a) => (next[a] || '') === level);

/** The sentences of "Nadaj dostęp" for `subject` = `{ subjectType, label, memberCount }`. */
export function grantImpact({ subject, next, topic }) {
  const lines = [];
  const allowed = actionsOf(next, 'allow');
  const denied = actionsOf(next, 'deny');
  const unset = actionsOf(next, '');
  const name = subject.label;
  if (allowed.length) {
    const doing = listFormat(allowed.map((a) => T(`access.can.${a}`)));
    if (subject.subjectType === 'group' && subject.memberCount != null) {
      lines.push(T('access.grant.impact_group', { count: fmtCount(subject.memberCount), n: Number(subject.memberCount), name, doing, topic }));
    } else {
      lines.push(T(`access.grant.impact_${subject.subjectType === 'addon' ? 'addon' : subject.subjectType === 'group' ? 'group_plain' : 'user'}`, { name, rights: listFormat(allowed.map((a) => T(`access.right.${a}`))), topic }));
    }
  }
  if (denied.length) lines.push(T('access.grant.impact_deny', { name, rights: listFormat(denied.map((a) => T(`access.right.${a}`))) }));
  if (unset.length) lines.push(T('access.grant.impact_unset', { rights: listFormat(unset.map((a) => T(`access.about.${a}`))) }));
  return lines;
}

const levelWord = (level) => T(level === 'allow' ? 'access.allowed' : level === 'deny' ? 'access.denied' : 'access.not_set');

/** The sentences of "Zmień dostęp": each changed right from what it was to what it becomes. */
export function changeImpact({ name, current, next }) {
  const lines = [];
  const changed = ACL_ACTIONS.filter((a) => (current[a] || '') !== (next[a] || ''));
  for (const a of changed) {
    lines.push(T('access.change.impact_line', { right: T(`access.right_title.${a}`), name, from: levelWord(current[a]), to: levelWord(next[a]) }));
    if (!next[a]) lines.push(T(`access.change.impact_role.${a}`));
    else lines.push(T(`access.change.impact_${next[a] === 'allow' ? 'allow' : 'deny'}`, { doing: T(`access.can.${a}`) }));
  }
  if (changed.length && changed.length < ACL_ACTIONS.length) lines.push(T('access.change.impact_rest'));
  return lines;
}

/** "Anna Kowalska straci wpis w tym topiku: czytanie (pozwolono), …" and whether a "zabroniono" goes. */
export function removeFacts(row) {
  const parts = ACL_ACTIONS.map((a) => [a, rightOf(row, a)]).filter(([, v]) => v)
    .map(([a, v]) => T('access.remove.part', { right: T(`access.right.${a}`), level: levelWord(v) }));
  return {
    lead: T('access.remove.lead', { name: subjectTitle(row), parts: parts.join(', ') }),
    liftsDeny: ACL_ACTIONS.some((a) => rightOf(row, a) === 'deny'),
  };
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/**
 * The keys card's rows. `aclEntries` = the topic's entries (a key's message
 * rights are its `api_key` rows there); `keys` = the general keys with their
 * scopes (`{ keyId, name, lastUsedAtEpoch, scopes }`), or `null` for a reader
 * who is not the server administrator — then only the message rights are
 * known and nothing about the last use. A key is listed when it has a right
 * here. An entry naming a key that no longer exists (the server names every
 * existing key, so it has no label) makes a `gone` row: its rights do
 * nothing and `entries` lists the actions to clear. Each row: `{ keyId, name,
 * known, gone, rights, entries, lastUsedMs, otherScopes }`; gone rows last.
 */
export function keyRows({ aclEntries, keys, instanceId, orgId }) {
  const rows = new Map();
  const rowFor = (keyId, name) => {
    if (!rows.has(keyId)) rows.set(keyId, { keyId, name: name || null, known: false, gone: false, rights: { readMessages: false, writeMessages: false, readSchemas: false, writeSchemas: false }, entries: [], lastUsedMs: undefined, otherScopes: 0 });
    return rows.get(keyId);
  };
  for (const e of aclEntries || []) {
    if (e.subjectType !== 'api_key') continue;
    if (!e.subjectLabel) {
      const row = rowFor(e.subjectId, null);
      row.gone = true;
      row.entries.push(e.action);
      if (e.accessLevel === 'allow' && e.action === 'read') row.rights.readMessages = true;
      if (e.accessLevel === 'allow' && e.action === 'write') row.rights.writeMessages = true;
      continue;
    }
    if (e.accessLevel !== 'allow') continue;
    if (e.action === 'read') rowFor(e.subjectId, e.subjectLabel).rights.readMessages = true;
    else if (e.action === 'write') rowFor(e.subjectId, e.subjectLabel).rights.writeMessages = true;
  }
  if (keys) {
    const schemaId = busSchemaScopeId(instanceId, orgId);
    for (const k of keys) {
      const scopes = k.scopes || [];
      const schemaRight = (action) => scopes.some((s) => s.resourceType === BUS_SCHEMA_REGISTRY && s.resourceId === schemaId && s.action === action && s.accessLevel === 'allow');
      const readSchemas = schemaRight('read');
      const writeSchemas = schemaRight('write');
      if (!rows.has(k.keyId) && !readSchemas && !writeSchemas) continue;
      const row = rowFor(k.keyId, k.name);
      row.name = k.name;
      row.known = true;
      row.rights.readSchemas = readSchemas;
      row.rights.writeSchemas = writeSchemas;
      row.lastUsedMs = k.lastUsedAtEpoch != null ? Number(k.lastUsedAtEpoch) * 1000 : null;
      const shown = Object.values(row.rights).filter(Boolean).length;
      row.otherScopes = Math.max(0, scopes.filter((s) => s.accessLevel === 'allow').length - shown);
    }
  }
  return [...rows.values()].sort((a, b) => Number(a.gone) - Number(b.gone)
    || String(a.name ?? '').localeCompare(String(b.name ?? ''), I18n.getLanguage()));
}

/** The requests that clear every entry a gone key left on the topic: allows first, bans last. */
export function buildGoneKeyClearRequests(instanceId, topic, row, aclEntries) {
  const base = { instanceId, topic, subjectType: 'api_key', subjectId: row.keyId };
  const steps = (aclEntries || [])
    .filter((e) => e.subjectType === 'api_key' && e.subjectId === row.keyId)
    .map((e) => ({ kind: e.accessLevel === 'deny' ? 'clear-deny' : 'clear-allow', request: { ...base, accessLevel: 'clear', action: e.action } }));
  return ordered(steps);
}

/** The rights of a key, as the chips and sentences name them. */
export function keyRightNames(rights) {
  return KEY_RIGHTS.filter((r) => rights[r]).map((r) => T(`access.keys.right.${r}`));
}

/** "przed chwilą", "dziś 14:02", "wczoraj 20:11", a date, or "jeszcze nie". */
export function lastUsedText(ms, nowMs = Date.now()) {
  if (ms == null) return T('access.keys.never');
  if (nowMs - ms < JUST_NOW_MS) return T('access.keys.just_now');
  return fmtDayTime(ms, nowMs);
}

/** A table row of the keys' card; a gone key's rights are greyed, they do nothing. */
export function keyTableRow(row, nowMs = Date.now()) {
  const names = keyRightNames(row.rights);
  const tone = row.gone ? 'neutral' : 'ok';
  return {
    who: row.gone
      ? `<span class="tf-table__cell-title">${escapeHtml(T('access.keys.gone'))}</span><div class="tf-table__cell-sub">${escapeHtml(T('access.keys.gone_sub'))}</div>`
      : `<span class="tf-table__cell-title">${escapeHtml(row.name)}</span><div class="tf-table__cell-sub">${escapeHtml(T('access.keys.kind'))}</div>`,
    rights: names.map((n) => `<span class="tf-chip tf-chip--outline ${tone}">${escapeHtml(n)}</span>`).join(' '),
    used: escapeHtml(row.known ? lastUsedText(row.lastUsedMs, nowMs) : '—'),
    _key: row.keyId,
  };
}

/** The scope a key right is stored under. */
function keyScope(right, { instanceId, orgId, topic }) {
  const messages = right === 'readMessages' || right === 'writeMessages';
  return {
    resourceType: messages ? 'topic' : BUS_SCHEMA_REGISTRY,
    resourceId: messages ? topicScopeId(instanceId, orgId, topic) : busSchemaScopeId(instanceId, orgId),
    action: right.startsWith('read') ? 'read' : 'write',
  };
}

/** `ApiKeyCreate` of a general key with the checked rights as its scopes. */
export function buildKeyCreateRequest(where, { name, rights }) {
  return {
    name: name.trim(),
    keyType: 'general',
    scopeResources: KEY_RIGHTS.filter((r) => rights[r]).map((r) => keyScope(r, where)),
  };
}

/** The `ApiKeyScopeSet`/`ApiKeyScopeClear` requests that turn `current` rights into `next`. */
export function buildKeyRightsRequests(where, keyUid, current, next) {
  return KEY_RIGHTS.filter((r) => Boolean(current[r]) !== Boolean(next[r])).map((r) => {
    const scope = keyScope(r, where);
    return next[r]
      ? { kind: 'apiKeyScopeSetRequest', payload: { keyUid, ...scope, accessLevel: 'allow' } }
      : { kind: 'apiKeyScopeClearRequest', payload: { keyUid, ...scope } };
  });
}

/** The address a system reads and writes the topic's messages at. */
export function recordsUrl(origin, instanceId, topic, orgId) {
  return `${origin}/v1/bus/instances/${encodeURIComponent(instanceId)}/topics/${encodeURIComponent(topic)}/records?org_id=${encodeURIComponent(orgId)}`;
}

/** "Co się stanie" of "Wydaj klucz". */
export function issueImpact({ name, rights, topic }) {
  return [T('access.keys.issue.impact', { name, rights: listFormat(keyRightNames(rights).map((r) => r.toLocaleLowerCase(I18n.getLanguage()))), topic })];
}

/** "Co się stanie" of "Prawa klucza". */
export function keyRightsImpact({ name, current, next }) {
  const lower = (list) => listFormat(list.map((r) => T(`access.keys.right.${r}`).toLocaleLowerCase(I18n.getLanguage())));
  const added = KEY_RIGHTS.filter((r) => next[r] && !current[r]);
  const removed = KEY_RIGHTS.filter((r) => !next[r] && current[r]);
  const lines = [];
  if (added.length) lines.push(T('access.keys.rights.impact_added', { name, rights: lower(added) }));
  if (removed.length) lines.push(T('access.keys.rights.impact_removed', { name, rights: lower(removed) }));
  if (!KEY_RIGHTS.some((r) => next[r])) lines.push(T('access.keys.rights.impact_none'));
  if (added.length || removed.length) lines.push(T('access.keys.rights.impact_rest'));
  return lines;
}

// ---------------------------------------------------------------------------
// The directory behind "Nadaj dostęp"
// ---------------------------------------------------------------------------

/**
 * Asks the directory so that only the newest question lands: switching from
 * "Grupa" to "Użytkownik" or typing on must not let a slower answer to the
 * earlier question fill the list. `fetch({ kind, query })` asks the server;
 * `apply({ kind, query, entries, truncated } | { kind, query, error })` takes
 * the answer; `alive()` is false once the window is gone.
 */
export function directoryLoader({ fetch, apply, alive = () => true }) {
  let latest = 0;
  return async function load(kind, query = '') {
    const turn = ++latest;
    let outcome;
    try {
      const res = await fetch({ kind, query });
      outcome = { kind, query, entries: res?.entries || [], truncated: Boolean(res?.truncated) };
    } catch (error) {
      outcome = { kind, query, error };
    }
    if (turn === latest && alive()) apply(outcome);
  };
}

/** The directory entries that can still be picked: those without an entry here. */
export function pickable(entries, rows) {
  const taken = new Set((rows || []).map((r) => `${r.subjectType}:${r.subjectId}`));
  return (entries || []).filter((e) => !taken.has(`${e.subjectType}:${e.subjectId}`));
}

/** How a directory entry is offered: a group with its size. */
export function pickLabel(entry) {
  if (entry.subjectType === 'group' && entry.memberCount != null) {
    return T('access.grant.group_option', { name: entry.label, count: fmtCount(entry.memberCount), n: Number(entry.memberCount) });
  }
  return entry.label;
}

// ---------------------------------------------------------------------------
// The section
// ---------------------------------------------------------------------------

/** Entries shown in the menu's counter: people, groups and addons plus keys. */
export function accessCount(data, where) {
  if (!data?.acl) return null;
  return subjectRows(data.acl).length + keyRows({ aclEntries: data.acl, keys: data.keys, ...where }).filter((k) => !k.gone).length;
}

function skeleton(siteAdmin) {
  return `
    <div data-role="notice"></div>
    <div class="section-card" data-role="subjects-card">
      <div class="section-card-head">
        <div class="title">${sprite('users')} ${escapeHtml(T('access.subjects_title'))} <span data-role="subjects-count"></span></div>
        <div class="actions"><tf-button variant="primary" size="sm" icon="plus" data-go="access-grant">${escapeHtml(T('access.grant.button'))}</tf-button></div>
      </div>
      <div class="section-sub">${escapeHtml(T('access.explain'))}</div>
      <div data-role="subjects-state"></div>
      <tf-table data-role="subjects">
        <tf-column key="who" label="${escapeAttr(T('access.col_who'))}" renderer="html" fill></tf-column>
        <tf-column key="read" label="${escapeAttr(T('access.right_title.read'))}" renderer="html"></tf-column>
        <tf-column key="write" label="${escapeAttr(T('access.right_title.write'))}" renderer="html"></tf-column>
        <tf-column key="admin" label="${escapeAttr(T('access.right_title.admin'))}" renderer="html"></tf-column>
      </tf-table>
      <div class="tb-table-footer" data-role="subjects-foot"><span>${escapeHtml(T('access.legend'))}</span></div>
    </div>
    <div class="section-card" data-role="keys-card">
      <div class="section-card-head">
        <div class="title">${sprite('key')} ${escapeHtml(T('access.keys.title'))} <span data-role="keys-count"></span></div>
        <div class="actions" data-role="keys-actions"></div>
      </div>
      <div class="section-sub" data-role="keys-sub"></div>
      <div data-role="keys-state"></div>
      <tf-table data-role="keys">
        <tf-column key="who" label="${escapeAttr(T('access.keys.col_system'))}" renderer="html" fill></tf-column>
        <tf-column key="rights" label="${escapeAttr(T('access.keys.col_rights'))}" renderer="html"></tf-column>
        ${siteAdmin ? `<tf-column key="used" label="${escapeAttr(T('access.keys.col_used'))}" renderer="html"></tf-column>` : ''}
      </tf-table>
      <div class="tb-who-can" data-role="keys-who" hidden>${sprite('lock')}<span>${escapeHtml(T('access.keys.site_admin_only'))}</span></div>
    </div>`;
}

function chipCount(host, value) {
  if (value == null) { patchHtml(host, ''); return; }
  patchHtml(host, '<tf-chip size="sm" variant="outline" status="neutral"></tf-chip>');
  setAttr(host.firstElementChild, 'label', fmtCount(value));
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

function stateHtml(loading, error) {
  if (error) return `<div class="tb-state tb-state--error">${sprite('alert')}<span>${escapeHtml(error)}</span><tf-button variant="secondary" size="sm" icon="refresh" data-go="access-reload">${escapeHtml(T('shell.retry'))}</tf-button></div>`;
  if (loading) return `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`;
  return '';
}

/**
 * Paints the Dostęp section. `view` carries the page's `topic`, `capabilities`
 * (`isSiteAdmin`, `orgId`, `orgName`), `instanceId`, `instanceLabel`,
 * `notice`, `nowMs` and `accessData` = `{ acl, aclError, keys, keysError }`
 * (`acl` null while loading; `keys` null unless the reader is the server
 * administrator). Buttons call `ctx.go({ kind: 'access-grant' })`,
 * `{ kind: 'access-change' | 'access-remove', subject }`,
 * `{ kind: 'key-issue' }`, `{ kind: 'key-rights' | 'key-revoke', keyId }`
 * and `{ kind: 'access-reload' }`.
 */
export function paintAccessSection(host, view, ctx) {
  const siteAdmin = view.capabilities?.isSiteAdmin === true;
  // Only the server administrator is told when a key was last used, so the
  // column exists for them alone.
  if (host.__tbAccess !== `built:${siteAdmin}`) {
    host.__tbAccess = `built:${siteAdmin}`;
    patchHtml(host, skeleton(siteAdmin));
  }
  const data = view.accessData || {};
  const where = { instanceId: view.instanceId, orgId: view.capabilities?.orgId || '', topic: view.topic.name };
  patchHtml(host.querySelector('[data-role="notice"]'), view.notice
    ? `<tf-alert tone="${escapeAttr(view.notice.tone || 'success')}" title="${escapeAttr(view.notice.title)}" message="${escapeAttr(view.notice.text || '')}"></tf-alert>`
    : '');

  const rows = data.acl ? subjectRows(data.acl) : [];
  const subjectsTable = host.querySelector('[data-role="subjects"]');
  let subjectsState = stateHtml(!data.acl && !data.aclError, !data.acl ? data.aclError : null);
  if (data.acl && !rows.length) subjectsState = `<tf-empty-state badge icon="users" title="${escapeAttr(T('access.empty_title'))}" message="${escapeAttr(T('access.empty_text'))}"></tf-empty-state>`;
  patchHtml(host.querySelector('[data-role="subjects-state"]'), subjectsState);
  subjectsTable.hidden = rows.length === 0;
  host.querySelector('[data-role="subjects-foot"]').hidden = rows.length === 0;
  chipCount(host.querySelector('[data-role="subjects-count"]'), data.acl ? rows.length : null);
  if (!subjectsTable.__tbWired) {
    subjectsTable.__tbWired = true;
    subjectsTable.rowActionsKey = (row) => row._key;
    subjectsTable.rowActions = (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      return rowButtons([
        ['change', T('access.change.button'), () => ctx.go({ kind: 'access-change', subject: live()._key })],
        ['remove', T('access.remove.button'), () => ctx.go({ kind: 'access-remove', subject: live()._key })],
      ]);
    };
  }
  setRowsIfChanged(subjectsTable, rows.map(subjectTableRow));

  const keys = data.acl ? keyRows({ aclEntries: data.acl, keys: siteAdmin ? data.keys : null, ...where }) : [];
  const keysLoading = !data.acl || (siteAdmin && !data.keys && !data.keysError);
  const keysError = data.aclError && !data.acl ? null : (siteAdmin ? data.keysError : null);
  const keysTable = host.querySelector('[data-role="keys"]');
  let keysState = stateHtml(keysLoading && !keysError && data.acl != null, keysError);
  if (!keysLoading && !keysError && !keys.length) keysState = `<div class="tb-state">${escapeHtml(T('access.keys.empty'))}</div>`;
  patchHtml(host.querySelector('[data-role="keys-state"]'), keysState);
  keysTable.hidden = keys.length === 0 || keysLoading;
  chipCount(host.querySelector('[data-role="keys-count"]'), keysLoading ? null : keys.filter((k) => !k.gone).length);
  patchHtml(host.querySelector('[data-role="keys-actions"]'), siteAdmin
    ? `<tf-button variant="secondary" size="sm" icon="key" data-go="key-issue" ${where.orgId ? '' : 'disabled'}>${escapeHtml(T('access.keys.issue.button'))}</tf-button>`
    : '');
  const org = view.capabilities?.orgName || T('access.keys.org_unknown');
  const sub = host.querySelector('[data-role="keys-sub"]');
  const subText = siteAdmin
    ? T('access.keys.sub', { instance: view.instanceLabel, org })
    : T('access.keys.sub_reader', { instance: view.instanceLabel, org });
  if (sub.textContent !== subText) sub.textContent = subText;
  host.querySelector('[data-role="keys-who"]').hidden = siteAdmin;
  if (!keysTable.__tbWired) {
    keysTable.__tbWired = true;
    keysTable.rowActionsKey = (row) => `${row._key}|${row._known}|${row._gone}`;
    keysTable.rowActions = siteAdmin ? (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      if (row._gone) return rowButtons([['key-clear', T('access.keys.clear.button'), () => ctx.go({ kind: 'key-clear', keyId: live()._key })]]);
      if (!row._known) return null;
      return rowButtons([
        ['key-rights', T('access.change.button'), () => ctx.go({ kind: 'key-rights', keyId: live()._key })],
        ['key-revoke', T('access.keys.revoke.button'), () => ctx.go({ kind: 'key-revoke', keyId: live()._key })],
      ]);
    } : null;
  }
  const nowMs = view.nowMs ?? Date.now();
  setRowsIfChanged(keysTable, keys.map((k) => ({ ...keyTableRow(k, nowMs), _known: k.known, _gone: k.gone })));
}

// ---------------------------------------------------------------------------
// The windows over people, groups and addons
// ---------------------------------------------------------------------------

function rightsFieldsHtml(topic, values) {
  return `
    <div class="field">
      <label>${escapeHtml(T('access.rights_label', { topic }))}</label>
      <div class="tb-rights">
        ${ACL_ACTIONS.map((a) => `
          <div class="tb-right-row">
            <div class="tb-right-name"><b>${escapeHtml(T(`access.right_title.${a}`))}</b><span>${escapeHtml(T(`access.right_hint.${a}`))}</span></div>
            <tf-segmented size="md" data-right="${a}" value="${escapeAttr(values[a] || '')}" aria-label="${escapeAttr(T(`access.right_title.${a}`))}"></tf-segmented>
          </div>`).join('')}
      </div>
    </div>`;
}

function wireRights(win, sync, values) {
  for (const a of ACL_ACTIONS) {
    const seg = win.querySelector(`tf-segmented[data-right="${a}"]`);
    seg.setOptions([
      { value: 'allow', label: T('access.level.allow'), variant: 'ok' },
      { value: 'deny', label: T('access.level.deny'), variant: 'err' },
      { value: '', label: T('access.level.unset') },
    ], values[a] || '');
    seg.addEventListener('change', sync);
  }
}

const readRights = (win) => Object.fromEntries(ACL_ACTIONS.map((a) => [a, win.querySelector(`tf-segmented[data-right="${a}"]`).value || '']));

/** Runs the requests one after another; the first refusal stops the rest. */
async function runAll(requests, send) {
  for (const r of requests) await send(r);
}

/**
 * "Nadaj dostęp": who (a user, a group or an addon from the directory, only
 * those without an entry here), then the three rights. `ctx` = `{ instanceId,
 * topic, rows, directory({ kind, query }), setAcl(request), describeError,
 * onSaved(notice) }`.
 */
export function openAccessGrant(ctx) {
  const { topic } = ctx;
  let kind = 'group';
  let query = '';
  let found = { kind, entries: null, truncated: false, error: null };
  const current = { subject: '', read: '', write: '', admin: '' };
  const entryOf = (value) => (found.entries || []).find((e) => `${e.subjectType}:${e.subjectId}` === value) || null;
  return openChangeWindow({
    title: T('access.grant.window_title', { topic }),
    icon: 'plus',
    width: 620,
    cls: 'tb-change-window tb-access-window',
    current,
    saveLabel: T('access.grant.button'),
    saveIcon: 'plus',
    fields: () => `
      <div class="field">
        <label>${escapeHtml(T('access.grant.who'))}</label>
        <tf-segmented size="md" data-role="kind" value="${kind}" aria-label="${escapeAttr(T('access.grant.who'))}"></tf-segmented>
      </div>
      <tf-searchbox data-role="query" debounce="250" placeholder="${escapeAttr(T('access.grant.search'))}"></tf-searchbox>
      <tf-select data-role="subject" label="${escapeAttr(T(`access.grant.pick.${kind}`))}"></tf-select>
      <div class="muted" data-role="pick-note" aria-live="polite"></div>
      ${rightsFieldsHtml(topic, { read: 'allow' })}`,
    wire: (w, sync) => {
      const kindSeg = w.querySelector('[data-role="kind"]');
      kindSeg.setOptions(SUBJECT_KINDS.map((k) => ({ value: k, label: T(`access.kind.${k}`) })), kind);
      const select = w.querySelector('[data-role="subject"]');
      const note = w.querySelector('[data-role="pick-note"]');
      const search = w.querySelector('[data-role="query"]');
      const paintPick = () => {
        const options = found.entries ? pickable(found.entries, ctx.rows) : [];
        select.setAttribute('label', T(`access.grant.pick.${kind}`));
        // Nobody is chosen for the administrator: the first name of a list is not a decision.
        select.setOptions([{ value: '', label: T('hiding.add.choose') }, ...options.map((e) => ({ value: `${e.subjectType}:${e.subjectId}`, label: pickLabel(e) }))], '');
        select.toggleAttribute('disabled', options.length === 0);
        let text;
        if (found.error) text = ctx.describeError(found.error);
        else if (!found.entries) text = T('shell.loading');
        else if (!options.length) text = T(query ? 'access.grant.none_found' : `access.grant.none.${kind}`);
        else text = T(`access.grant.pick_hint.${kind}`) + (found.truncated ? ` ${T('access.grant.truncated')}` : '');
        note.textContent = text;
        sync();
      };
      const load = directoryLoader({
        fetch: ctx.directory,
        alive: () => w.isConnected,
        apply: (outcome) => {
          found = { kind: outcome.kind, entries: outcome.entries || null, truncated: Boolean(outcome.truncated), error: outcome.error || null };
          paintPick();
        },
      });
      const ask = () => {
        found = { kind, entries: null, truncated: false, error: null };
        paintPick();
        load(kind, query);
      };
      kindSeg.addEventListener('change', () => { kind = kindSeg.value; ask(); });
      search.addEventListener('search', (e) => { query = String(e.detail?.value ?? '').trim(); ask(); });
      select.addEventListener('change', sync);
      wireRights(w, sync, { read: 'allow' });
      ask();
    },
    draft: (w) => ({ subject: found.entries ? (w.querySelector('[data-role="subject"]').value || '') : '', ...readRights(w) }),
    problem: (d) => {
      if (!d.subject || !entryOf(d.subject)) return T('access.grant.pick_first');
      if (!ACL_ACTIONS.some((a) => d[a])) return T('access.grant.pick_right');
      return null;
    },
    impact: (d) => {
      const e = entryOf(d.subject);
      return grantImpact({ subject: { subjectType: e.subjectType, label: e.label, memberCount: e.memberCount ?? null }, next: d, topic });
    },
    save: (d) => {
      const e = entryOf(d.subject);
      return runAll(buildAclRequests(ctx.instanceId, topic, { subjectType: e.subjectType, subjectId: e.subjectId }, null, d), ctx.setAcl);
    },
    describeError: ctx.describeError,
    onSaved: (d) => {
      const e = entryOf(d.subject);
      const lines = grantImpact({ subject: { subjectType: e.subjectType, label: e.label, memberCount: e.memberCount ?? null }, next: d, topic });
      ctx.onSaved({ title: T('access.grant.saved_title'), text: lines.join(' ') });
    },
  });
}

/** "Zmień dostęp" of one row: the subject fixed, the three rights as they are now. */
export function openAccessChange(row, ctx) {
  const { topic } = ctx;
  const current = currentRights(row);
  const name = subjectTitle(row);
  return openChangeWindow({
    title: T('access.change.window_title', { name }),
    icon: 'edit',
    width: 620,
    cls: 'tb-change-window tb-access-window',
    current,
    fields: () => `
      <div class="field">
        <label>${escapeHtml(T('access.grant.who'))}</label>
        <div class="tb-explain-box"><b>${escapeHtml(name)}</b><div class="muted">${escapeHtml(subjectSub(row))}</div></div>
      </div>
      ${rightsFieldsHtml(topic, current)}`,
    wire: (w, sync) => wireRights(w, sync, current),
    draft: readRights,
    impact: (d) => changeImpact({ name, current, next: d }),
    save: (d) => runAll(buildAclRequests(ctx.instanceId, topic, row, row, d), ctx.setAcl),
    describeError: ctx.describeError,
    onSaved: (d) => ctx.onSaved({ title: T('access.change.saved_title'), text: changeImpact({ name, current, next: d }).join(' ') }),
  });
}

/** "Usuń" one row's entries, with the warning when a "zabroniono" goes. */
export function openAccessRemove(row, ctx) {
  const { topic } = ctx;
  const name = subjectTitle(row);
  const { lead, liftsDeny } = removeFacts(row);
  return openConfirmWindow({
    title: T('access.remove.window_title', { name }),
    icon: 'trash',
    cls: 'tb-unp-confirm tb-access-window',
    lead: `<div class="tb-explain-box">${escapeHtml(lead)}</div>${liftsDeny ? `<div class="tb-danger-box" data-role="lifts-deny">${sprite('alert')}<div>${escapeHtml(T('access.remove.lifts_deny'))}</div></div>` : ''}`,
    impactTitle: T('access.remove.impact_title'),
    impact: [T('access.remove.impact', { topic })],
    button: T('access.remove.confirm'),
    buttonIcon: 'trash',
    danger: true,
    run: () => runAll(buildAclRemoveRequests(ctx.instanceId, topic, row), ctx.setAcl),
    describeError: ctx.describeError,
    onDone: () => ctx.onSaved({ title: T('access.remove.saved_title'), text: T('access.remove.saved_text', { name }) }),
  });
}

// ---------------------------------------------------------------------------
// The windows over keys (server administrator only)
// ---------------------------------------------------------------------------

function keyRightsFieldsHtml(topic, values) {
  return `
    <div class="field">
      <label>${escapeHtml(T('access.keys.col_rights'))}</label>
      <div class="tb-key-rights">
        ${KEY_RIGHTS.map((r) => `<tf-checkbox data-key-right="${r}" label="${escapeAttr(T(`access.keys.right.${r}`))}" description="${escapeAttr(T(`access.keys.right_hint.${r}`, { topic }))}" ${values[r] ? 'checked' : ''}></tf-checkbox>`).join('')}
      </div>
    </div>`;
}

const readKeyRights = (win) => Object.fromEntries(KEY_RIGHTS.map((r) => [r, win.querySelector(`tf-checkbox[data-key-right="${r}"]`).hasAttribute('checked')]));

function wireKeyRights(win, sync) {
  for (const r of KEY_RIGHTS) win.querySelector(`tf-checkbox[data-key-right="${r}"]`).addEventListener('change', sync);
}

/**
 * "Wydaj klucz": a name and the rights; on success the secret is shown once
 * (`openKeyIssued`). `ctx` = `{ where: { instanceId, orgId, topic },
 * instanceLabel, orgName, create(request), describeError, origin, onIssued(notice) }`.
 */
export function openKeyIssue(ctx) {
  const { topic } = ctx.where;
  const current = { name: '', readMessages: false, writeMessages: false, readSchemas: false, writeSchemas: false };
  return openChangeWindow({
    title: T('access.keys.issue.window_title'),
    icon: 'key',
    width: 600,
    cls: 'tb-change-window tb-access-window',
    current,
    saveLabel: T('access.keys.issue.button'),
    saveIcon: 'key',
    fields: () => `
      <tf-input data-role="name" label="${escapeAttr(T('access.keys.issue.name_label'))}" hint="${escapeAttr(T('access.keys.issue.name_hint'))}" maxlength="${KEY_NAME_MAX}"></tf-input>
      ${keyRightsFieldsHtml(topic, {})}`,
    wire: (w, sync) => {
      w.querySelector('[data-role="name"]').addEventListener('input', sync);
      wireKeyRights(w, sync);
    },
    draft: (w) => ({ name: String(w.querySelector('[data-role="name"]').value || ''), ...readKeyRights(w) }),
    problem: (d) => {
      if (!d.name.trim()) return T('access.keys.issue.name_first');
      if (d.name.trim().length > KEY_NAME_MAX) return T('access.keys.issue.name_too_long', { max: fmtCount(KEY_NAME_MAX) });
      if (!KEY_RIGHTS.some((r) => d[r])) return T('access.keys.issue.right_first');
      return null;
    },
    impact: (d) => issueImpact({ name: d.name.trim(), rights: d, topic }),
    save: (d) => ctx.create(buildKeyCreateRequest(ctx.where, { name: d.name, rights: d })),
    describeError: ctx.describeError,
    onSaved: (d, result) => {
      const name = d.name.trim();
      const rights = listFormat(keyRightNames(d).map((r) => r.toLocaleLowerCase(I18n.getLanguage())));
      openKeyIssued({ ...ctx, name, rights: d, keyId: result?.keyId, token: result?.token });
      ctx.onIssued({ title: T('access.keys.issue.saved_title'), text: T('access.keys.issue.saved_text', { name, rights }) });
    },
  });
}

/**
 * The one window that shows a new key's secret, with what the system needs
 * to use it: the address and how it reads (`k:<key id>` consumer groups).
 */
export function openKeyIssued({ where, instanceLabel, orgName, origin, name, rights, keyId, token, copy }) {
  const url = recordsUrl(origin, where.instanceId, where.topic, where.orgId);
  const rightsText = `${listFormat(keyRightNames(rights).map((r) => r.toLocaleLowerCase(I18n.getLanguage())))} · ${T('access.keys.issued.topic', { topic: where.topic })}`;
  const group = `k:${keyId || ''}`;
  const kv = [
    [T('access.keys.issued.k_key'), `<span class="mono" data-role="token">${escapeHtml(token || '')}</span>`],
    [T('access.keys.issued.k_system'), escapeHtml(name)],
    [T('access.keys.issued.k_instance'), escapeHtml(`${instanceLabel} · ${orgName || T('access.keys.org_unknown')}`)],
    [T('access.keys.issued.k_rights'), escapeHtml(rightsText)],
    [T('access.keys.issued.k_address'), `<span class="mono" data-role="url">${escapeHtml(url)}</span>`],
  ];
  const win = windowEl({ title: T('access.keys.issued.window_title', { name }), icon: 'key', width: 640, cls: 'tb-access-window tb-key-issued' });
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="tb-danger-box">${sprite('alert')}<div>${escapeHtml(T('access.keys.issued.once'))}</div></div>
      <div class="tb-kv-grid">${kv.map(([k, v]) => `<div class="k">${escapeHtml(k)}</div><div class="v">${v}</div>`).join('')}</div>
      <details class="tb-tech" data-role="developer">
        <summary>${escapeHtml(T('access.keys.issued.developer'))}</summary>
        <div class="tb-will-happen">${sprite('info')}<div>${escapeHtml(T('access.keys.issued.how', { group }))}</div></div>
        <div class="tb-key-group"><span class="mono" data-role="group">${escapeHtml(group)}</span>
          <tf-button variant="ghost" size="sm" icon="copy" data-act="copy-group">${escapeHtml(T('access.keys.issued.copy_group'))}</tf-button></div>
      </details>
      <div class="tb-window-error" role="alert" data-role="discard" hidden>${sprite('alert')}<div>${escapeHtml(T('access.keys.issued.close_confirm'))}</div></div>
      <div class="muted" data-role="copied" aria-live="polite"></div>
    </div>
    <div slot="footer">
      <tf-button variant="secondary" icon="copy" data-act="copy-key">${escapeHtml(T('access.keys.issued.copy_key'))}</tf-button>
      <tf-button variant="secondary" icon="copy" data-act="copy-url">${escapeHtml(T('access.keys.issued.copy_url'))}</tf-button>
      <tf-button variant="primary" data-act="done">${escapeHtml(T('access.keys.issued.done'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const copied = win.querySelector('[data-role="copied"]');
  const discard = win.querySelector('[data-role="discard"]');
  let keyCopied = false;
  // The secret is shown once: Escape or the close button ask once before it
  // goes, unless it was copied ("Gotowe" is the explicit way out).
  win.addEventListener('close-request', (e) => {
    if (keyCopied || !discard.hidden) return;
    e.preventDefault();
    discard.hidden = false;
  });
  const texts = {
    'copy-key': ['access.keys.issued.copied_key', () => token],
    'copy-url': ['access.keys.issued.copied_url', () => url],
    'copy-group': ['access.keys.issued.copied_group', () => group],
  };
  win.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn) return;
    if (btn.dataset.act === 'done') { win.close(true); return; }
    const entry = texts[btn.dataset.act];
    if (!entry) return;
    const ok = await (copy || copyToClipboard)(entry[1]());
    if (ok && btn.dataset.act === 'copy-key') keyCopied = true;
    copied.textContent = ok ? T(entry[0]) : T('access.keys.issued.copy_failed');
  });
  return win;
}

async function copyToClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** "Prawa klucza": the four rights, what changes, the key itself unchanged. */
export function openKeyRights(row, ctx) {
  const { topic } = ctx.where;
  const current = { ...row.rights };
  const name = row.name;
  return openChangeWindow({
    title: T('access.keys.rights.window_title', { name }),
    icon: 'key',
    width: 600,
    cls: 'tb-change-window tb-access-window',
    current,
    fields: () => `
      <div class="field">
        <label>${escapeHtml(T('access.keys.col_system'))}</label>
        <div class="tb-explain-box"><b>${escapeHtml(name)}</b><div class="muted">${escapeHtml(T('access.keys.kind'))}</div></div>
      </div>
      ${keyRightsFieldsHtml(topic, current)}`,
    wire: (w, sync) => wireKeyRights(w, sync),
    draft: readKeyRights,
    impact: (d) => keyRightsImpact({ name, current, next: d }),
    save: (d) => runAll(buildKeyRightsRequests(ctx.where, row.keyId, current, d), ctx.scope),
    describeError: ctx.describeError,
    onSaved: (d) => ctx.onSaved({ title: T('access.keys.rights.saved_title'), text: keyRightsImpact({ name, current, next: d }).join(' ') }),
  });
}

/** "Usuń prawa" a gone key left on the topic: they do nothing, they only go. */
export function openGoneKeyClear(row, ctx) {
  return openConfirmWindow({
    title: T('access.keys.clear.window_title'),
    icon: 'trash',
    cls: 'tb-unp-confirm tb-access-window',
    lead: `<div class="tb-explain-box">${escapeHtml(T('access.keys.clear.lead'))}</div>`,
    impactTitle: T('access.remove.impact_title'),
    impact: [T('access.keys.clear.impact', { topic: ctx.where.topic })],
    button: T('access.keys.clear.button'),
    buttonIcon: 'trash',
    danger: true,
    run: () => runAll(buildGoneKeyClearRequests(ctx.where.instanceId, ctx.where.topic, row, ctx.aclEntries), ctx.setAcl),
    describeError: ctx.describeError,
    onDone: () => ctx.onSaved({ title: T('access.keys.clear.saved_title'), text: T('access.keys.clear.saved_text') }),
  });
}

/** "Unieważnij" a key: at once and for good, whatever else it could do. */
export function openKeyRevoke(row, ctx) {
  const name = row.name;
  const rights = listFormat(keyRightNames(row.rights).map((r) => r.toLocaleLowerCase(I18n.getLanguage())));
  const lines = [T('access.keys.revoke.impact', { name, rights })];
  if (row.otherScopes > 0) lines.push(T('access.keys.revoke.impact_other', { count: fmtCount(row.otherScopes), n: row.otherScopes }));
  return openConfirmWindow({
    title: T('access.keys.revoke.window_title', { name }),
    icon: 'key',
    cls: 'tb-unp-confirm tb-access-window',
    lead: `<div class="tb-danger-box">${sprite('alert')}<div>${escapeHtml(T('access.keys.revoke.lead'))}</div></div>`,
    impactTitle: T('access.keys.revoke.impact_title'),
    impact: lines,
    audit: T('access.keys.revoke.audit'),
    button: T('access.keys.revoke.button'),
    buttonIcon: 'close',
    danger: true,
    run: () => ctx.revoke({ keyId: row.keyId }),
    describeError: ctx.describeError,
    onDone: () => ctx.onSaved({ title: T('access.keys.revoke.saved_title'), text: T('access.keys.revoke.saved_text', { name }) }),
  });
}
