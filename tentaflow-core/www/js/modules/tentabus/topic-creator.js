// ===== File: modules/tentabus/topic-creator.js — "Nowy topik" (T04): a three-step window over the topic list =====
//
// Step 1 names the topic, picks what the programs will send (the kinds this
// server can read, `BusCapabilities.contentTypes`) and how many partitions it
// has; step 2 how long it keeps messages and how safely it writes them, with
// the sentence about copies taken from the server's own resolution
// (`defaultReplicationFactor` / `nodeCount` — the number `create_topic`
// will use, so the window never promises a different one); step 3 the
// message pattern, offered only among patterns that fit the chosen content
// and are not withdrawn, and the summary. The number of copies is never sent:
// the instance picks it. Same window, header, progress rail and footer as the
// TentaNas wizards.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, fmtCount, fmtRetention, contentKind, contentTypeLabel } from '/js/modules/tentabus/format.js';
import { schemaFormatLabel } from '/js/modules/tentabus/schemas.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-choice-card.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

// Mirrors `bus::topics::validate_user_topic_name`: the server stays the
// authority, this only answers while the name is being typed. 121 characters,
// not 127: the topic's dead-letter topic `__dlq.<name>` must fit in 127 too.
export const TOPIC_NAME_MAX = 121;
const TOPIC_NAME_RE = new RegExp(`^[a-z0-9][a-z0-9.-]{1,${TOPIC_NAME_MAX - 1}}$`);

export const PARTITIONS_MIN = 1;
export const PARTITIONS_MAX = 256;
export const RETENTION_DAYS = [7, 30, 90, 365];
const DAY_MS = 86_400_000;
const GIB = 1024 ** 3;
export const PARTITION_LIMITS_GB = [0, 8, 16];

const CONTENT_ICONS = { json: 'file-code', xml: 'file-text', hl7v2: 'activity', binary: 'layers' };

// Which pattern formats describe which content: a JSON Schema checks JSON,
// an XSD checks XML, an HL7 v2 profile checks HL7 v2, and Avro / Protobuf /
// Thrift describe binary payloads.
const SCHEMA_TYPES_BY_KIND = {
  json: ['json_schema'],
  xml: ['xsd'],
  hl7v2: ['hl7v2_profile'],
  binary: ['avro', 'protobuf', 'thrift'],
};

/**
 * Why a name cannot be used, or `null` when it can: `empty`, `reserved`
 * (the broker's own `__` prefix), `invalid` (characters or length), `taken`
 * (a topic of that name is already listed).
 */
export function topicNameProblem(name, existingNames = []) {
  const s = String(name || '').trim();
  if (!s) return 'empty';
  if (s.startsWith('__')) return 'reserved';
  if (!TOPIC_NAME_RE.test(s)) return 'invalid';
  if (existingNames.includes(s)) return 'taken';
  return null;
}

/** The content kinds the creator offers: those the server reads that this screen can name, in its order. */
export function creatorContentTypes(contentTypes) {
  const seen = new Set();
  const out = [];
  for (const ct of contentTypes || []) {
    const kind = contentKind(ct);
    if (!kind || seen.has(kind)) continue;
    seen.add(kind);
    out.push({ contentType: ct, kind });
  }
  return out;
}

/**
 * Patterns a topic of `contentType` may be checked with: the right format
 * for that content, one this server can validate with (`schemaTypes`), and
 * not withdrawn — a withdrawn pattern cannot be chosen for a new topic.
 */
export function compatibleSchemas(subjects, contentType, schemaTypes) {
  const allowed = new Set(SCHEMA_TYPES_BY_KIND[contentKind(contentType)] || []);
  const validates = new Set(schemaTypes || []);
  return (subjects || [])
    .filter((s) => s.deprecatedAtMs == null && allowed.has(s.schemaType) && validates.has(s.schemaType))
    .sort((a, b) => a.subject.localeCompare(b.subject));
}

/** The pattern formats that fit `contentType`, in words ("JSON Schema"). */
export function compatibleFormatsLabel(contentType) {
  return (SCHEMA_TYPES_BY_KIND[contentKind(contentType)] || []).map(schemaFormatLabel).join(', ');
}

// The server's rule for a new topic's copies: one per node in the
// environment, at most this many (`bus::default_rf_for_nodes`).
export const MAX_COPIES = 3;

/**
 * How many copies the new topic gets and why, from the server's resolution:
 * `many` (one per node), `capped` (more nodes than the most copies a topic
 * gets), `single` (one node, one copy) or `unknown` (a server that does not
 * say, or a number that rule does not explain — then no reason is given).
 */
export function copiesPlan({ defaultReplicationFactor, nodeCount } = {}) {
  const count = Number(defaultReplicationFactor) || 0;
  const nodes = Number(nodeCount) || 0;
  if (count <= 0) return { kind: 'unknown', count: 0, nodes };
  if (count === 1 && nodes <= 1) return { kind: 'single', count, nodes: 1 };
  if (count >= nodes) return { kind: 'many', count, nodes: count };
  if (count === MAX_COPIES) return { kind: 'capped', count, nodes };
  return { kind: 'unknown', count: 0, nodes };
}

/** "3 kopie" — the bold part of the copies sentence. */
export function copiesCount(plan) {
  return T('topics.creator.copies_count', { count: fmtCount(plan.count), n: plan.count });
}

/** "bo w tej instancji są 3 nody" — the reason part, shared by step 2 and the summary. */
export function copiesReason(plan) {
  return T(`topics.creator.copies_reason_${plan.kind}`, { nodes: fmtCount(plan.nodes), n: plan.nodes, max: fmtCount(MAX_COPIES), m: MAX_COPIES });
}

/** The partition count typed in the field, or `null` when it is not a whole number in range. */
export function partitionsValue(raw) {
  const text = String(raw ?? '').trim();
  if (!/^\d+$/.test(text)) return null;
  const n = Number(text);
  return n >= PARTITIONS_MIN && n <= PARTITIONS_MAX ? n : null;
}

/** A fresh draft: JSON-like first kind, three partitions, 30 days, normal durability, no pattern yet. */
export function newDraft(kinds) {
  return {
    name: '',
    contentType: kinds[0]?.contentType || '',
    partitions: 3,
    retentionDays: 30,
    limitGb: 0,
    durabilityClass: 'standard',
    validate: false,
    schemaId: '',
    onMismatch: 'dlq',
  };
}

/**
 * The create request for a draft. Only what the window asked is sent; the
 * copies and everything the window does not show stay the server's defaults.
 * A pattern is bound only when checking is on and one was picked.
 */
export function buildTopicCreateRequest(instanceId, draft) {
  const options = {
    partitions: Number(draft.partitions),
    retentionMs: Number(draft.retentionDays) * DAY_MS,
    cleanupPolicy: 'delete',
    durabilityClass: draft.durabilityClass === 'critical' ? 'critical' : 'standard',
  };
  if (draft.contentType) options.contentType = draft.contentType;
  if (Number(draft.limitGb) > 0) options.retentionBytesPerPartition = Number(draft.limitGb) * GIB;
  if (draft.validate && draft.schemaId) {
    options.schemaId = draft.schemaId;
    options.validation = draft.onMismatch === 'warn' ? 'warn' : 'dlq';
  }
  return { instanceId, name: String(draft.name).trim(), options };
}

/**
 * Opens the creator. `capabilities` = the instance's `BusCapabilities`,
 * `subjects` = its message patterns, or `null` when they have not been read
 * — the window then asks `reloadSubjects()` (resolving the list, or
 * throwing), and when that fails step 3 says so and offers to ask again. `existingNames` = the listed topics.
 * `create(request)` sends the request (it may throw; the window stays open
 * with `describeError(err)`), `onCreated({ name, schemaId, plan })` runs
 * after the window closed on success.
 */
export function openTopicCreator({ instanceLabel, capabilities = {}, subjects = [], reloadSubjects, existingNames = [], instanceId, create, describeError = (e) => String(e?.message || e), onCreated }) {
  const kinds = creatorContentTypes(capabilities.contentTypes);
  const plan = copiesPlan(capabilities);
  const draft = newDraft(kinds);
  const state = { step: 0, nameTouched: false, busy: false, error: '', subjects, reloading: false };
  const steps = [T('topics.creator.step_1'), T('topics.creator.step_2'), T('topics.creator.step_3')];

  const win = document.createElement('tf-window');
  win.className = 'tb-window tb-creator';
  win.setAttribute('title', T('topics.creator.title'));
  win.setAttribute('icon', 'share');
  win.setAttribute('buttons', 'close');
  win.setAttribute('modal', '');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '720');
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const schemasFor = () => compatibleSchemas(state.subjects, draft.contentType, capabilities.schemaTypes);
  const nameProblem = () => topicNameProblem(draft.name, existingNames);
  const canProceed = () => {
    if (state.busy) return false;
    if (state.step === 0) return nameProblem() === null && draft.partitions != null;
    if (state.step === 2) return !draft.validate || Boolean(draft.schemaId);
    return true;
  };

  // The heading shows the name only once it is one a topic can have.
  const headingName = () => (['taken', null].includes(nameProblem()) ? draft.name.trim() : T('topics.creator.heading_placeholder'));

  const header = () => `
    <div class="install-header">
      <div class="big-ico">${sprite('broadcast')}</div>
      <div class="install-header-meta">
        <h1><span class="mono" data-role="heading">${escapeHtml(headingName())}</span> <span class="version">${escapeHtml(T('topics.creator.instance_tag', { name: instanceLabel }))}</span></h1>
        <div class="sub">${escapeHtml(T('topics.creator.sub'))}</div>
      </div>
    </div>
    <div class="install-progress">${steps.map((s, i) => `<div class="install-step ${i === state.step ? 'active' : i < state.step ? 'done' : ''}"><span class="num">${i < state.step ? sprite('check') : i + 1}</span><span class="label">${escapeHtml(s)}</span></div>`).join('')}</div>`;

  const nameError = () => {
    const problem = nameProblem();
    if (!problem || (!state.nameTouched && problem === 'empty')) return '';
    return T(`topics.creator.name_${problem}`);
  };

  const stepName = () => `
    <h2 class="wizard-section-title">${escapeHtml(T('topics.creator.step_1'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(T('topics.creator.step_1_sub'))}</p>
    <div class="form-grid-2">
      <tf-input id="tb-cr-name" class="tb-mono-input" label="${escapeAttr(T('topics.creator.name_label'))}" placeholder="${escapeAttr(T('topics.creator.name_placeholder'))}" autocomplete="off" spellcheck="false" autocapitalize="off" value="${escapeAttr(draft.name)}" hint="${escapeAttr(T('topics.creator.name_hint'))}" error="${escapeAttr(nameError())}"></tf-input>
      <tf-input id="tb-cr-partitions" type="number" inputmode="numeric" stepper min="${PARTITIONS_MIN}" max="${PARTITIONS_MAX}" step="1" label="${escapeAttr(T('topics.creator.partitions_label'))}" value="${escapeAttr(draft.partitions ?? '')}" hint="${escapeAttr(T('topics.creator.partitions_hint'))}" stepper-dec-label="${escapeAttr(T('topics.creator.partitions_less'))}" stepper-inc-label="${escapeAttr(T('topics.creator.partitions_more'))}"></tf-input>
    </div>
    ${kinds.length ? `
      <div class="field mt-md">
        <label id="tb-cr-kind-label">${escapeHtml(T('topics.creator.kind_label'))}</label>
        <tf-choice-group id="tb-cr-kind" value="${escapeAttr(draft.contentType)}" columns="${kinds.length}" aria-label="${escapeAttr(T('topics.creator.kind_label'))}">
          ${kinds.map((k) => `<tf-choice-card value="${escapeAttr(k.contentType)}" icon="${CONTENT_ICONS[k.kind]}" heading="${escapeAttr(contentTypeLabel(k.contentType))}" description="${escapeAttr(T(`topics.creator.kind_desc.${k.kind}`))}"></tf-choice-card>`).join('')}
        </tf-choice-group>
      </div>` : ''}`;

  const copiesBox = () => `
    <div class="tb-copies-box">
      <div class="cb-num">${plan.count ? escapeHtml(fmtCount(plan.count)) : sprite('branch')}</div>
      <div class="cb-txt">${plan.kind === 'unknown'
        ? escapeHtml(T('topics.creator.copies_unknown'))
        : `<b>${escapeHtml(copiesCount(plan))}</b>, ${escapeHtml(copiesReason(plan))}. ${escapeHtml(T(`topics.creator.copies_tail_${plan.kind === 'single' ? 'single' : 'many'}`))}`}</div>
    </div>`;

  const stepStorage = () => `
    <h2 class="wizard-section-title">${escapeHtml(T('topics.creator.step_2'))}</h2>
    <p class="wizard-section-sub">${escapeHtml(T('topics.creator.step_2_sub'))}</p>
    <div class="form-grid-2">
      <tf-select id="tb-cr-retention" label="${escapeAttr(T('topics.creator.retention_label'))}" hint="${escapeAttr(T('topics.creator.retention_hint'))}"></tf-select>
      <tf-select id="tb-cr-limit" label="${escapeAttr(T('topics.creator.limit_label'))}" hint="${escapeAttr(T('topics.creator.limit_hint'))}"></tf-select>
    </div>
    <div class="tb-stat-rows mt-md">
      <div class="sr"><span class="k">${sprite('lock')} ${escapeHtml(T('topics.creator.cleanup_label'))}</span><span class="v">${escapeHtml(T('topics.creator.cleanup_value'))}</span></div>
    </div>
    <div class="field mt-md">
      <label>${escapeHtml(T('topics.creator.copies_label'))}</label>
      ${copiesBox()}
    </div>
    <div class="field mt-md">
      <label>${escapeHtml(T('topics.creator.durability_label'))}</label>
      <tf-choice-group id="tb-cr-durability" value="${escapeAttr(draft.durabilityClass)}" columns="2" aria-label="${escapeAttr(T('topics.creator.durability_label'))}">
        <tf-choice-card value="standard" icon="zap" heading="${escapeAttr(T('topics.creator.durability_standard'))}" description="${escapeAttr(T('topics.creator.durability_standard_desc'))}"></tf-choice-card>
        <tf-choice-card value="critical" icon="shield" heading="${escapeAttr(T('topics.creator.durability_critical'))}" description="${escapeAttr(T('topics.creator.durability_critical_desc'))}"></tf-choice-card>
      </tf-choice-group>
    </div>`;

  const summaryRetention = () => {
    const parts = [T('topics.creator.summary_retention', { period: fmtRetention(draft.retentionDays * DAY_MS) })];
    if (draft.limitGb > 0) parts.push(T('topics.creator.summary_limit', { size: T('topics.creator.limit_gb', { count: fmtCount(draft.limitGb) }) }));
    return parts.join('; ');
  };

  const summarySchema = () => {
    if (!draft.validate || !draft.schemaId) return T('topics.creator.summary_no_schema');
    const version = Number(schemasFor().find((x) => x.subject === draft.schemaId)?.latestVersion) || 0;
    const schema = version > 0
      ? T('topics.creator.summary_schema_versioned', { name: draft.schemaId, version: fmtCount(version) })
      : draft.schemaId;
    return T(`topics.creator.summary_schema_${draft.onMismatch === 'warn' ? 'warn' : 'dlq'}`, { schema });
  };

  const summary = () => {
    const rows = [
      [T('topics.creator.summary_name'), `<span class="mono">${escapeHtml(draft.name.trim())}</span>`],
      [T('topics.creator.kind_label'), escapeHtml(contentTypeLabel(draft.contentType) || T('topics.creator.summary_kind_default'))],
      [T('topics.creator.partitions_label'), escapeHtml(fmtCount(draft.partitions))],
      [T('topics.creator.summary_storage'), escapeHtml(summaryRetention())],
      [T('topics.creator.copies_label'), escapeHtml(plan.kind === 'unknown' ? T('topics.creator.summary_copies_unknown') : `${fmtCount(plan.count)}, ${copiesReason(plan)}`)],
      [T('topics.creator.durability_label'), escapeHtml(T(`topics.creator.summary_durability_${draft.durabilityClass}`))],
      [T('topics.creator.summary_schema'), escapeHtml(summarySchema())],
    ];
    return `<div class="tb-kv-grid mt-md">${rows.map(([k, v]) => `<div class="k">${escapeHtml(k)}</div><div class="v">${v}</div>`).join('')}</div>`;
  };

  const stepSchema = () => {
    const available = schemasFor();
    let body;
    if (state.subjects == null && state.reloading) {
      body = `<div class="tb-explain-box" role="status">${escapeHtml(T('topics.creator.schemas_loading'))}</div>`;
    } else if (state.subjects == null) {
      body = `
        <div class="tb-explain-box tb-explain-box--error" role="alert">
          <span>${escapeHtml(T('topics.creator.schemas_failed', { instance: instanceLabel }))}</span>
          ${reloadSubjects ? `<tf-button variant="secondary" size="sm" icon="refresh" data-act="reload-subjects" ${state.reloading ? 'disabled' : ''}>${escapeHtml(T('shell.retry'))}</tf-button>` : ''}
        </div>`;
    } else if (!available.length) {
      const key = state.subjects.length ? 'no_schema_for_kind' : 'no_schema_at_all';
      body = `<div class="tb-explain-box">${escapeHtml(T(`topics.creator.${key}`, { instance: instanceLabel, kind: contentTypeLabel(draft.contentType) || T('topics.creator.summary_kind_default') }))}</div>`;
    } else {
      body = `
        <div class="tb-toggle-card">
          <tf-toggle id="tb-cr-validate" ${draft.validate ? 'checked' : ''} aria-label="${escapeAttr(T('topics.creator.validate_label'))}"></tf-toggle>
          <div class="tc-text"><div class="tc-name">${escapeHtml(T('topics.creator.validate_label'))}</div><div class="tc-sub">${escapeHtml(T('topics.creator.validate_sub'))}</div></div>
        </div>
        <div class="form-grid-2 mt-md" ${draft.validate ? '' : 'hidden'}>
          <tf-select id="tb-cr-schema" label="${escapeAttr(T('topics.creator.schema_label'))}" hint="${escapeAttr(T('topics.creator.schema_hint', { formats: compatibleFormatsLabel(draft.contentType), kind: contentTypeLabel(draft.contentType) }))}"></tf-select>
          <tf-select id="tb-cr-mismatch" label="${escapeAttr(T('topics.creator.mismatch_label'))}"></tf-select>
        </div>`;
    }
    return `
      <h2 class="wizard-section-title">${escapeHtml(T('topics.creator.step_3_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('topics.creator.step_3_sub'))}</p>
      ${body}
      ${summary()}`;
  };

  const footer = () => {
    const last = state.step === 2;
    return `
      <tf-button variant="ghost" data-act="cancel" ${state.busy ? 'disabled' : ''}>${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="ghost" icon="chevron-left" data-act="back" ${state.step === 0 || state.busy ? 'disabled' : ''}>${escapeHtml(I18n.t('common.back'))}</tf-button>
      <tf-button variant="primary" ${last ? 'icon="check"' : 'trailing-icon="chevron-right"'} data-act="next" ${canProceed() ? '' : 'disabled'}>${escapeHtml(last ? T('topics.creator.create') : I18n.t('common.next'))}</tf-button>`;
  };

  const syncNext = () => {
    const btn = win.querySelector('[data-act="next"]');
    if (!btn) return;
    if (canProceed()) btn.removeAttribute('disabled');
    else btn.setAttribute('disabled', '');
  };

  const draw = () => {
    win.innerHTML = `
      <div slot="body">
        ${header()}
        <div class="install-step-body">${[stepName, stepStorage, stepSchema][state.step]()}</div>
        <div class="tb-window-error" role="alert" ${state.error ? '' : 'hidden'}>${sprite('alert')}<span>${escapeHtml(state.error)}</span></div>
      </div>
      <div slot="footer">${footer()}</div>`;
    wire();
  };

  const wire = () => {
    const name = win.querySelector('#tb-cr-name');
    if (name) {
      const onName = () => {
        draft.name = name.value.trim();
        state.nameTouched = true;
        const err = nameError();
        if (err) name.setAttribute('error', err);
        else name.removeAttribute('error');
        const heading = win.querySelector('[data-role="heading"]');
        if (heading) heading.textContent = headingName();
        syncNext();
      };
      name.addEventListener('input', onName);
      name.addEventListener('change', onName);
      name.addEventListener('keydown', (e) => { if (e.key === 'Enter' && canProceed()) advance(); });
    }
    const partitions = win.querySelector('#tb-cr-partitions');
    if (partitions) {
      const onPartitions = () => {
        draft.partitions = partitionsValue(partitions.value);
        if (draft.partitions == null) partitions.setAttribute('error', T('topics.creator.partitions_invalid', { min: PARTITIONS_MIN, max: PARTITIONS_MAX }));
        else partitions.removeAttribute('error');
        syncNext();
      };
      partitions.addEventListener('input', onPartitions);
      partitions.addEventListener('change', onPartitions);
    }
    win.querySelector('#tb-cr-kind')?.addEventListener('change', (e) => {
      draft.contentType = e.detail.value;
      // A pattern picked for another content kind no longer fits; with no
      // pattern for the new kind there is nothing to check against either.
      if (!schemasFor().some((s) => s.subject === draft.schemaId)) draft.schemaId = '';
      if (!schemasFor().length) draft.validate = false;
    });
    const retention = win.querySelector('#tb-cr-retention');
    if (retention) {
      retention.setOptions(RETENTION_DAYS.map((d) => ({ value: String(d), label: fmtRetention(d * DAY_MS) })), String(draft.retentionDays));
      retention.addEventListener('change', (e) => { draft.retentionDays = Number(e.detail.value); });
    }
    const limit = win.querySelector('#tb-cr-limit');
    if (limit) {
      limit.setOptions(PARTITION_LIMITS_GB.map((g) => ({ value: String(g), label: g ? T('topics.creator.limit_gb', { count: fmtCount(g) }) : T('topics.creator.limit_none') })), String(draft.limitGb));
      limit.addEventListener('change', (e) => { draft.limitGb = Number(e.detail.value); });
    }
    win.querySelector('#tb-cr-durability')?.addEventListener('change', (e) => { draft.durabilityClass = e.detail.value; });
    win.querySelector('#tb-cr-validate')?.addEventListener('change', (e) => {
      draft.validate = Boolean(e.detail?.checked ?? e.target.checked);
      if (draft.validate && !draft.schemaId) draft.schemaId = schemasFor()[0]?.subject || '';
      redraw('#tb-cr-validate');
    });
    const schema = win.querySelector('#tb-cr-schema');
    if (schema) {
      schema.setOptions(schemasFor().map((s) => ({ value: s.subject, label: `${s.subject} · ${schemaFormatLabel(s.schemaType)}` })), draft.schemaId);
      schema.addEventListener('change', (e) => { draft.schemaId = e.detail.value; redraw('#tb-cr-schema'); });
    }
    const mismatch = win.querySelector('#tb-cr-mismatch');
    if (mismatch) {
      mismatch.setOptions([{ value: 'dlq', label: T('topics.creator.mismatch_dlq') }, { value: 'warn', label: T('topics.creator.mismatch_warn') }], draft.onMismatch);
      mismatch.addEventListener('change', (e) => { draft.onMismatch = e.detail.value === 'warn' ? 'warn' : 'dlq'; redraw('#tb-cr-mismatch'); });
    }
  };

  // The summary follows every choice; the control that made it keeps the keyboard.
  const redraw = (id) => {
    draw();
    win.querySelector(`${id} select, ${id} input, ${id} button, ${id} [tabindex]`)?.focus();
  };

  const toStep = (step) => {
    state.step = step;
    state.error = '';
    draw();
    win.scrollBodyTop();
    win.focusFirst();
  };

  // Asks for the pattern list (again). Only step 3 shows it, so only there
  // is the window redrawn — elsewhere a redraw would take the field being
  // typed in away. `fromButton`: the retry button asked, so the keyboard
  // lands on what replaced it.
  const reload = async (fromButton = false) => {
    state.reloading = true;
    if (state.step === 2) draw();
    let loaded = null;
    try {
      loaded = await reloadSubjects();
    } catch {
      loaded = null;
    }
    state.reloading = false;
    if (!win.isConnected) return;
    state.subjects = Array.isArray(loaded) ? loaded : null;
    if (state.subjects && !draft.schemaId && schemasFor().length) {
      draft.validate = true;
      draft.schemaId = schemasFor()[0].subject;
    }
    if (state.step !== 2) return;
    if (fromButton) redraw(state.subjects ? '#tb-cr-validate' : '[data-act="reload-subjects"]');
    else draw();
  };

  const advance = async () => {
    if (!canProceed()) {
      if (state.step === 0) { state.nameTouched = true; draw(); }
      return;
    }
    state.error = '';
    if (state.step < 2) {
      if (state.step === 1 && schemasFor().length && !draft.schemaId) {
        draft.validate = true;
        draft.schemaId = schemasFor()[0].subject;
      }
      toStep(state.step + 1);
      return;
    }
    state.busy = true;
    draw();
    const request = buildTopicCreateRequest(instanceId, draft);
    try {
      await create(request);
    } catch (err) {
      state.busy = false;
      state.error = describeError(err);
      draw();
      return;
    }
    win.close(true);
    onCreated?.({ name: request.name, schemaId: request.options.schemaId || '', plan });
  };

  // While the request is out the window stays: closing it would hide the answer.
  win.addEventListener('close-request', (e) => { if (state.busy) e.preventDefault(); });
  win.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn || btn.hasAttribute('disabled')) return;
    const act = btn.dataset.act;
    if (act === 'cancel') win.close(true);
    else if (act === 'back' && state.step > 0) toStep(state.step - 1);
    else if (act === 'next') advance();
    else if (act === 'reload-subjects' && !state.reloading) reload(true);
  });

  draw();
  document.body.appendChild(win);
  // A list that has not arrived yet is asked for now, not reported as missing.
  if (state.subjects == null && reloadSubjects) reload();
  return win;
}
