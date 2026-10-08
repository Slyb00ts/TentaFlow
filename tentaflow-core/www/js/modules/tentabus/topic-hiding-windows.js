// ===== File: modules/tentabus/topic-hiding-windows.js — the windows of Ukrywanie danych: add, change and delete a rule =====
//
// One rule is one subject (a person, a group, an addon or everyone) in one
// direction (reading or writing). "Dodaj zasadę" picks the subject from the
// directory and the direction, "Zmień" keeps both and edits the fields,
// "Usuń" asks with what the rule does now. Every window says what will happen
// before it saves (`Co się stanie po zapisaniu`), and the fields are drawn
// like the access rights: the field and its name, one segmented control.
//
// Fields come from the topic (`source`, see `fieldSource`): a row per known
// field, and below them the typed-in ones — the server keeps only what is
// allowed, so a field the window does not list is hidden too, and the fields
// of a topic nobody listed are typed in as "the ones that stay visible".

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { T } from '/js/modules/tentabus/format.js';
import { openChangeWindow, openConfirmWindow } from '/js/modules/tentabus/windows.js';
import { directoryLoader, pickLabel } from '/js/modules/tentabus/topic-access.js';
import {
  SUBJECT_KINDS, DIRECTIONS, ANY_ID, whoTitle, whoSub, fieldPhrase, ruleFacts, actionOptions, blankForm, formFromRule, serializeForm,
  parseForm, formProblem, buildPolicyRequest, ruleImpact, policyKey,
} from '/js/modules/tentabus/topic-hiding.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-select.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-tag-input.js';

const typedHint = { hl7v2: 'hiding.extra.hint_hl7', xml: 'hiding.extra.hint_xml', json: 'hiding.extra.hint_json', binary: 'hiding.extra.hint_json' };

// ---------------------------------------------------------------------------
// The fields of a rule
// ---------------------------------------------------------------------------

function sourceNote(source) {
  if (source.mode === 'schema') return T('hiding.source.schema', { name: source.subject, version: source.version });
  if (source.mode === 'dictionary') return T('hiding.source.dictionary');
  return T(source.failed ? 'hiding.source.failed' : 'hiding.source.manual');
}

function rowHtml(field) {
  return `
    <div class="tb-right-row">
      <div class="tb-right-name"><b class="mono">${escapeHtml(field.name)}</b>${field.label ? `<span>${escapeHtml(field.label)}</span>` : ''}</div>
      <tf-segmented size="md" data-field="${escapeAttr(field.name)}" aria-label="${escapeAttr(field.name)}"></tf-segmented>
    </div>`;
}

function fieldsHtml(source) {
  const listed = source.fields.length > 0;
  return `
    <div class="field" data-role="fields-box"${listed ? '' : ' hidden'}>
      <label>${escapeHtml(T('hiding.fields_label'))}</label>
      <div class="tb-rights" data-role="rows"></div>
    </div>
    <div class="muted" data-role="source-note">${escapeHtml(sourceNote(source))}</div>
    <div data-role="extras"></div>
    <div class="muted" data-role="unlisted-note"></div>`;
}

function extraHtml(role, label, hint) {
  return `
    <div class="field">
      <label>${escapeHtml(label)}</label>
      <tf-tag-input dedupe data-role="${role}" aria-label="${escapeAttr(label)}"></tf-tag-input>
      <div class="muted">${escapeHtml(hint)}</div>
    </div>`;
}

/**
 * Draws the rows and the typed-in lists of `direction` with the values of
 * `form`; `sync` is called on every change.
 */
function paintFields(win, { direction, form, source, format, fieldActions, sync }) {
  const rows = win.querySelector('[data-role="rows"]');
  rows.innerHTML = source.fields.map(rowHtml).join('');
  for (const seg of rows.querySelectorAll('tf-segmented[data-field]')) {
    seg.setOptions(actionOptions(direction, fieldActions), form.actions[seg.getAttribute('data-field')]);
    seg.addEventListener('change', sync);
  }
  const listed = source.fields.length > 0;
  const read = direction === 'read';
  const hint = T(typedHint[format]);
  const extras = [extraHtml('extra-shown', T(`hiding.extra.${read ? 'shown' : 'allowed'}${listed ? '' : '_typed'}`), hint)];
  if (!read) extras.push(extraHtml('extra-required', T('hiding.extra.required'), hint));
  const host = win.querySelector('[data-role="extras"]');
  host.innerHTML = extras.join('');
  host.querySelector('[data-role="extra-shown"]').tags = form.extraShown;
  const required = host.querySelector('[data-role="extra-required"]');
  if (required) required.tags = form.extraRequired;
  for (const input of host.querySelectorAll('tf-tag-input')) input.addEventListener('change', sync);
  win.querySelector('[data-role="unlisted-note"]').textContent = listed ? T(`hiding.unlisted.${direction}`) : '';
}

/** The form the window holds now, as the string `serializeForm` makes of it. */
function readSerialized(win, source) {
  const actions = {};
  for (const seg of win.querySelectorAll('tf-segmented[data-field]')) actions[seg.getAttribute('data-field')] = seg.value || '';
  const tags = (role) => win.querySelector(`[data-role="${role}"]`)?.tags || [];
  return serializeForm({ actions, extraShown: tags('extra-shown'), extraRequired: tags('extra-required') }, source);
}

// ---------------------------------------------------------------------------
// Dodaj zasadę
// ---------------------------------------------------------------------------

/**
 * "Dodaj zasadę". `ctx` = `{ instanceId, topic, format, source, rules (the
 * stored rows), fieldActions, directory({ kind, query }), setPolicy(request),
 * describeError, onSaved(notice) }`.
 */
export function openHidingAdd(ctx) {
  const { topic, source, format } = ctx;
  const taken = new Set(ctx.rules.map(policyKey));
  let kind = 'group';
  let direction = 'read';
  let query = '';
  let found = { entries: null, truncated: false, error: null };
  const subjectOf = (value) => {
    if (value === `any:${ANY_ID}`) return { subjectType: 'any', subjectId: ANY_ID, label: T('hiding.everyone'), memberCount: null };
    const e = (found.entries || []).find((x) => `${x.subjectType}:${x.subjectId}` === value);
    return e ? { subjectType: e.subjectType, subjectId: e.subjectId, label: e.label, memberCount: e.memberCount ?? null } : null;
  };
  const free = (subject) => !taken.has(`${subject.subjectType}:${subject.subjectId}:${direction}`);
  const current = { subject: '', direction: 'read', form: serializeForm(blankForm('read', source), source) };
  const formOf = (d) => parseForm(d.form, source) ?? blankForm(d.direction, source);
  return openChangeWindow({
    title: T('hiding.add.window_title', { topic }),
    icon: 'plus',
    width: 660,
    cls: 'tb-change-window tb-access-window tb-hiding-window',
    current,
    saveLabel: T('hiding.add.save'),
    saveIcon: 'plus',
    fields: () => `
      <div class="field">
        <label>${escapeHtml(T('hiding.add.who'))}</label>
        <tf-segmented size="md" data-role="kind" aria-label="${escapeAttr(T('hiding.add.who'))}"></tf-segmented>
      </div>
      <div data-role="pick-box">
        <tf-searchbox data-role="query" debounce="250" placeholder="${escapeAttr(T('access.grant.search'))}"></tf-searchbox>
        <tf-select data-role="subject" label="${escapeAttr(T('access.grant.pick.group'))}"></tf-select>
      </div>
      <div class="muted" data-role="pick-note" aria-live="polite"></div>
      <div class="field">
        <label>${escapeHtml(T('hiding.add.when'))}</label>
        <tf-segmented size="md" data-role="direction" aria-label="${escapeAttr(T('hiding.add.when'))}"></tf-segmented>
      </div>
      ${fieldsHtml(source)}`,
    wire: (w, sync) => {
      const kindSeg = w.querySelector('[data-role="kind"]');
      kindSeg.setOptions(SUBJECT_KINDS.map((k) => ({ value: k, label: T(k === 'any' ? 'hiding.everyone' : `access.kind.${k}`) })), kind);
      const dirSeg = w.querySelector('[data-role="direction"]');
      dirSeg.setOptions(DIRECTIONS.map((d) => ({ value: d, label: T(`hiding.direction.${d}`) })), direction);
      const select = w.querySelector('[data-role="subject"]');
      const note = w.querySelector('[data-role="pick-note"]');
      const pickBox = w.querySelector('[data-role="pick-box"]');
      const search = w.querySelector('[data-role="query"]');
      const paintPick = () => {
        pickBox.hidden = kind === 'any';
        if (kind === 'any') {
          select.setOptions([{ value: `any:${ANY_ID}`, label: T('hiding.everyone') }], `any:${ANY_ID}`);
          note.textContent = free({ subjectType: 'any', subjectId: ANY_ID }) ? T('hiding.add.any_hint') : T('hiding.add.any_taken', { direction: T(`hiding.direction_of.${direction}`) });
          sync();
          return;
        }
        const options = found.entries ? found.entries.filter((e) => free(e)) : [];
        select.setAttribute('label', T(`access.grant.pick.${kind}`));
        select.setOptions(options.map((e) => ({ value: `${e.subjectType}:${e.subjectId}`, label: pickLabel(e) })), options[0] ? `${options[0].subjectType}:${options[0].subjectId}` : '');
        select.toggleAttribute('disabled', options.length === 0);
        let text;
        if (found.error) text = ctx.describeError(found.error);
        else if (!found.entries) text = T('shell.loading');
        else if (!options.length) text = T(query ? 'access.grant.none_found' : 'hiding.add.none_free');
        else text = T('hiding.add.pick_hint') + (found.truncated ? ` ${T('access.grant.truncated')}` : '');
        note.textContent = text;
        sync();
      };
      const load = directoryLoader({
        fetch: ctx.directory,
        alive: () => w.isConnected,
        apply: (outcome) => {
          found = { entries: outcome.entries || null, truncated: Boolean(outcome.truncated), error: outcome.error || null };
          paintPick();
        },
      });
      const ask = () => {
        found = { entries: null, truncated: false, error: null };
        paintPick();
        if (kind !== 'any') load(kind, query);
      };
      kindSeg.addEventListener('change', () => { kind = kindSeg.value; ask(); });
      search.addEventListener('search', (e) => { query = String(e.detail?.value ?? '').trim(); ask(); });
      select.addEventListener('change', sync);
      dirSeg.addEventListener('change', () => {
        direction = dirSeg.value;
        paintFields(w, { direction, form: blankForm(direction, source), source, format, fieldActions: ctx.fieldActions, sync });
        paintPick();
      });
      paintFields(w, { direction, form: blankForm(direction, source), source, format, fieldActions: ctx.fieldActions, sync });
      ask();
    },
    draft: (w) => ({
      subject: kind === 'any' ? `any:${ANY_ID}` : (found.entries ? (w.querySelector('[data-role="subject"]').value || '') : ''),
      direction,
      form: readSerialized(w, source),
    }),
    problem: (d) => {
      const subject = subjectOf(d.subject);
      if (!subject) return T('hiding.add.pick_first');
      if (!free(subject)) return T('hiding.add.taken', { direction: T(`hiding.direction_of.${d.direction}`) });
      return formProblem({ direction: d.direction, form: formOf(d), source, format });
    },
    impact: (d) => ruleImpact({ who: subjectOf(d.subject).label, direction: d.direction, form: formOf(d), current: null, source }),
    save: (d) => ctx.setPolicy(buildPolicyRequest({ instanceId: ctx.instanceId, topic, subject: subjectOf(d.subject), direction: d.direction, form: formOf(d), source })),
    describeError: ctx.describeError,
    onSaved: (d) => {
      const subject = subjectOf(d.subject);
      ctx.onSaved({
        title: T('hiding.add.saved_title'),
        text: ruleImpact({ who: subject.label, direction: d.direction, form: formOf(d), current: null, source }).join(' '),
      });
    },
  });
}

// ---------------------------------------------------------------------------
// Zmień
// ---------------------------------------------------------------------------

/** "Zmień" one rule: the subject and the direction fixed, the fields as they are now. `ctx` as `openHidingAdd`'s. */
export function openHidingChange(row, ctx) {
  const { topic, source, format } = ctx;
  const stored = formFromRule(row, source);
  const who = whoTitle(row);
  const current = { form: serializeForm(stored, source) };
  const formOf = (d) => parseForm(d.form, source) ?? stored;
  return openChangeWindow({
    title: T('hiding.change.window_title', { name: who }),
    icon: 'edit',
    width: 660,
    cls: 'tb-change-window tb-access-window tb-hiding-window',
    current,
    fields: () => `
      <div class="field">
        <label>${escapeHtml(T('hiding.add.who'))}</label>
        <div class="tb-explain-box"><b>${escapeHtml(who)}</b><div class="muted">${escapeHtml(whoSub(row))}</div></div>
      </div>
      <div class="field">
        <label>${escapeHtml(T('hiding.add.when'))}</label>
        <div class="tb-explain-box"><b>${escapeHtml(T(`hiding.direction.${row.direction}`))}</b><div class="muted">${escapeHtml(T('hiding.change.fixed'))}</div></div>
      </div>
      ${fieldsHtml(source)}`,
    wire: (w, sync) => paintFields(w, { direction: row.direction, form: stored, source, format, fieldActions: ctx.fieldActions, sync }),
    draft: (w) => ({ form: readSerialized(w, source) }),
    problem: (d) => formProblem({ direction: row.direction, form: formOf(d), source, format }),
    impact: (d) => ruleImpact({ who, direction: row.direction, form: formOf(d), current: stored, source }),
    save: (d) => ctx.setPolicy(buildPolicyRequest({
      instanceId: ctx.instanceId, topic, subject: { subjectType: row.subjectType, subjectId: row.subjectId }, direction: row.direction, form: formOf(d), source,
    })),
    describeError: ctx.describeError,
    onSaved: (d) => ctx.onSaved({
      title: T('hiding.change.saved_title'),
      text: ruleImpact({ who, direction: row.direction, form: formOf(d), current: stored, source }).join(' '),
    }),
  });
}

// ---------------------------------------------------------------------------
// Usuń
// ---------------------------------------------------------------------------

/** The sentence that says what the rule does now. */
export function removeLead(row, source) {
  const who = whoTitle(row);
  const facts = ruleFacts(row, source);
  const names = (list) => list.map((n) => fieldPhrase(source, n)).join(', ');
  if (row.direction === 'read') {
    if (!facts.known) return facts.visible.length ? T('hiding.remove.lead_read_only', { who, fields: facts.visible.join(', ') }) : T('hiding.remove.lead_read_nothing', { who });
    return facts.hidden.length ? T('hiding.remove.lead_read', { who, fields: names(facts.hidden) }) : T('hiding.remove.lead_read_none', { who });
  }
  const parts = [];
  if (facts.known && facts.forbidden.length) parts.push(T('hiding.remove.part_forbidden', { fields: names(facts.forbidden) }));
  if (!facts.known) parts.push(facts.visible.length ? T('hiding.remove.part_only', { fields: facts.visible.join(', ') }) : T('hiding.remove.part_nothing'));
  if (facts.required.length) parts.push(T('hiding.remove.part_required', { fields: names(facts.required) }));
  return T('hiding.remove.lead_write', { who, parts: parts.length ? parts.join('; ') : T('hiding.remove.part_none') });
}

/** What changes for the subject, and for keys, once the rule is gone. */
export function removeImpact(row, source, topic, rules) {
  const who = whoTitle(row);
  const lines = [];
  if (row.subjectType === 'any') {
    lines.push(T(`hiding.remove.impact_any_${row.direction}`, { topic }));
    const othersRemain = rules.some((r) => r.direction === row.direction && r.key !== row.key);
    if (othersRemain) lines.push(T('hiding.remove.impact_keys', { action: T(`hiding.direction_do.${row.direction}`) }));
  } else {
    lines.push(T(`hiding.remove.impact_${row.direction}`, { who, topic }));
  }
  return lines;
}

/** "Usuń" one rule. `ctx` = `{ instanceId, topic, source, rules, deleteRule(request), describeError, onSaved(notice) }`. */
export function openHidingRemove(row, ctx) {
  const { topic, source } = ctx;
  const who = whoTitle(row);
  return openConfirmWindow({
    title: T('hiding.remove.window_title', { name: who }),
    icon: 'trash',
    cls: 'tb-unp-confirm tb-access-window',
    lead: `<div class="tb-explain-box">${escapeHtml(removeLead(row, source))}</div>`,
    impactTitle: T('hiding.remove.impact_title'),
    impact: removeImpact(row, source, topic, ctx.rules),
    button: T('hiding.remove.confirm'),
    buttonIcon: 'trash',
    danger: true,
    run: () => ctx.deleteRule({ instanceId: ctx.instanceId, topic, subjectType: row.subjectType, subjectId: row.subjectId, direction: row.direction }),
    describeError: ctx.describeError,
    onDone: () => ctx.onSaved({ title: T('hiding.remove.saved_title'), text: T('hiding.remove.saved_text', { who, direction: T(`hiding.direction_of.${row.direction}`) }) }),
  });
}
