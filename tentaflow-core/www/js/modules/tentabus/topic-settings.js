// ===== File: modules/tentabus/topic-settings.js — a topic's Ustawienia section: values to read and the four "Zmień" windows =====
//
// The page shows each setting as "label — value" with one sentence of what it
// means; a value the page does not change carries a lock and the reason. Each
// card's "Zmień" opens a modal tf-window with only that card's settings and a
// "Co się stanie po zapisaniu" line computed from what the server will really
// do with the change (see the notes at each impact function). The save sends
// one `TopicUpdateRequest` with only the fields the window changed; the caller
// reloads the topic and shows "Zapisano …" with the new value.
//
// Server facts the wording rests on (tentaflow-core/src/bus):
//   - retention: the sweeper runs every 5 minutes on every node, deletes whole
//     sealed segment files whose last write is older than the retention, and
//     never the active one (`retention.rs::sweep_partition`);
//   - partitions only grow; added ones are placed like the first ones, keyed
//     messages hash over the new count, and an open consumer keeps the
//     partition list it opened with until it reconnects;
//   - `acks` decides what a write waits for from the next publish on;
//   - the retry pause is handed to the consumer's program with each reported
//     failure, doubling per attempt up to one minute, ±20 %
//     (`dlq::compute_backoff_ms`);
//   - `warn` validation accepts the message and only logs on the node;
//   - the durability class is fixed into a partition when it opens, so the
//     page does not offer to change it.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, fmtCount, fmtBytes, fmtRetention, fmtDuration, contentTypeLabel } from '/js/modules/tentabus/format.js';
import { compatibleSchemas, compatibleFormatsLabel } from '/js/modules/tentabus/topic-creator.js';
import { schemaFormatLabel } from '/js/modules/tentabus/schemas.js';
import { patchHtml } from '/js/lib/dom-patch.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-alert.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

const DAY_MS = 86_400_000;
const GIB = 1024 ** 3;
export const RETENTION_DAYS = [1, 3, 7, 14, 30, 90, 180, 365];
export const LIMIT_GB = [1, 2, 4, 8, 10, 16, 32, 64];
export const BACKOFF_MS = [1000, 2000, 5000, 10_000, 30_000, 60_000];
export const PARTITIONS_MAX = 256;
export const ATTEMPTS_MIN = 1;
export const ATTEMPTS_MAX = 100;
/// `topics::DEFAULT_RETRY_BACKOFF_CAP_MS`: no pause the broker hands out is longer.
export const BACKOFF_CAP_MS = 60_000;
/// How often every node's retention sweeper runs (`bus::native` DEFAULT_RETENTION_INTERVAL).
export const SWEEP_MINUTES = 5;
export const ACKS = ['leader', 'quorum', 'all'];
export const VALIDATION_MODES = ['dlq', 'warn', 'off'];

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/** Choices for a list that must also offer the current value when it is not a standard one. */
function withCurrent(values, current) {
  return values.includes(current) ? values : [...values, current].sort((a, b) => a - b);
}

export function retentionChoices(currentMs) {
  return withCurrent(RETENTION_DAYS.map((d) => d * DAY_MS), Number(currentMs));
}

export function limitChoices(currentBytes) {
  return withCurrent(LIMIT_GB.map((g) => g * GIB), Number(currentBytes));
}

export function backoffChoices(currentMs) {
  return withCurrent(BACKOFF_MS, Number(currentMs));
}

/** Writes needed for `acks=quorum` at `rf` copies: a majority (`election::min_isr_required`). */
export function quorumOf(rf) {
  return Math.floor(Number(rf) / 2) + 1;
}

/**
 * Pauses the consumer's program is handed between `attempts` tries, first
 * pause `baseMs`, doubling, capped at one minute — `attempts - 1` values.
 */
export function backoffSchedule(baseMs, attempts) {
  const out = [];
  for (let i = 0; i < Math.max(0, Number(attempts) - 1); i += 1) {
    out.push(Math.min(BACKOFF_CAP_MS, Number(baseMs) * 2 ** i));
  }
  return out;
}

/**
 * What the partitions of a topic tell about its data: the bytes they hold on
 * this node, the largest one and the time of the oldest message still kept.
 * Unknown parts stay `null`.
 */
export function storageFacts(partitions) {
  const list = partitions || [];
  if (!list.length) return { bytes: null, largest: null, oldestMs: null };
  const times = list.map((p) => p.earliestTimestampMs).filter((t) => t != null && Number.isFinite(Number(t))).map(Number);
  return {
    bytes: list.reduce((s, p) => s + (Number(p.sizeBytes) || 0), 0),
    largest: list.reduce((m, p) => Math.max(m, Number(p.sizeBytes) || 0), 0),
    oldestMs: times.length ? Math.min(...times) : null,
  };
}

/**
 * "Co się stanie" of the retention window, as sentences. Shortening removes
 * roughly the share of the data older than the new period — traffic is
 * assumed even over time, hence "ok." — at the next sweep.
 */
export function retentionImpact({ current, next, facts, nowMs = Date.now() }) {
  const lines = [];
  const period = fmtRetention(next.retentionMs);
  if (next.retentionMs < current.retentionMs) {
    const age = facts.oldestMs != null ? nowMs - facts.oldestMs : null;
    if (age != null && age <= next.retentionMs) {
      lines.push(T('settings.retention.impact_nothing_yet', { period }));
    } else if (age != null && facts.bytes > 0) {
      const share = (age - next.retentionMs) / age;
      lines.push(T('settings.retention.impact_shorter_size', {
        period,
        minutes: fmtCount(SWEEP_MINUTES),
        size: fmtBytes(facts.bytes * share),
        total: fmtBytes(facts.bytes),
      }));
    } else {
      lines.push(T('settings.retention.impact_shorter', { period, minutes: fmtCount(SWEEP_MINUTES) }));
    }
    // Only messages that go away are out of a reader's reach.
    if (!(age != null && age <= next.retentionMs)) lines.push(T('settings.retention.impact_no_rewind'));
  } else if (next.retentionMs > current.retentionMs) {
    lines.push(T('settings.retention.impact_longer', { period }));
  }
  const limit = fmtBytes(next.limitBytes);
  if (next.limitBytes < current.limitBytes) {
    lines.push(facts.largest != null && facts.largest > next.limitBytes
      ? T('settings.retention.impact_limit_exceeded', { limit, minutes: fmtCount(SWEEP_MINUTES) })
      : T('settings.retention.impact_limit_lower', { limit }));
  } else if (next.limitBytes > current.limitBytes) {
    lines.push(T('settings.retention.impact_limit_higher', { limit }));
  }
  return lines;
}

/** What `acks` makes a write wait for at `rf` copies, in words. */
export function acksHint(acks, rf) {
  if (Number(rf) <= 1) return T('settings.write.acks_hint_single');
  if (acks === 'quorum') return T('settings.write.acks_hint_quorum', { q: fmtCount(quorumOf(rf)), rf: fmtCount(rf) });
  if (acks === 'all') return T('settings.write.acks_hint_all');
  return T('settings.write.acks_hint_leader');
}

/** "Co się stanie" of the write window. */
export function writeImpact({ current, next, rf }) {
  const lines = [];
  if (next.partitions > current.partitions) {
    lines.push(T('settings.write.impact_partitions', { count: fmtCount(next.partitions), n: next.partitions }));
  }
  if (next.acks !== current.acks) {
    if (Number(rf) <= 1) lines.push(T('settings.write.impact_acks_single'));
    else if (next.acks === 'quorum') lines.push(T('settings.write.impact_acks_quorum', { q: fmtCount(quorumOf(rf)), rf: fmtCount(rf) }));
    else if (next.acks === 'all') lines.push(T('settings.write.impact_acks_all'));
    else lines.push(T('settings.write.impact_acks_leader'));
  }
  if (next.compression !== current.compression) {
    lines.push(T(next.compression === 'none' ? 'settings.write.impact_compression_off' : 'settings.write.impact_compression_on'));
  }
  return lines;
}

/** "ok. 2 s, 4 s i 8 s" — the pauses of a retry schedule. */
export function pausesText(schedule) {
  const list = new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(schedule.map(fmtDuration));
  return list;
}

/** "Co się stanie" of the retry window. */
export function retryImpact({ current, next }) {
  const lines = [];
  if (next.attempts !== current.attempts) {
    lines.push(next.attempts === 1
      ? T('settings.retry.impact_attempts_one')
      : T('settings.retry.impact_attempts', { count: fmtCount(next.attempts), n: next.attempts, old: fmtCount(current.attempts), o: current.attempts }));
  }
  if (next.attempts > 1 && (next.attempts !== current.attempts || next.backoffMs !== current.backoffMs)) {
    lines.push(T('settings.retry.impact_pauses', { pauses: pausesText(backoffSchedule(next.backoffMs, next.attempts)) }));
  }
  return lines;
}

/** "Co się stanie" of the pattern window. */
export function patternImpact({ current, next, versionOf }) {
  const lines = [];
  if (next.schemaId === current.schemaId && next.validation === current.validation) return lines;
  if (!next.schemaId) {
    lines.push(T(current.schemaId && current.validation !== 'off' ? 'settings.pattern.impact_none' : 'settings.pattern.impact_none_quiet'));
    return lines;
  }
  const version = versionOf(next.schemaId);
  const name = version ? T('settings.pattern.name_version', { name: next.schemaId, version: fmtCount(version) }) : next.schemaId;
  if (next.validation === 'off') lines.push(T('settings.pattern.impact_off', { name }));
  else {
    lines.push(T(`settings.pattern.impact_${next.validation}`, { name }));
    lines.push(T('settings.pattern.impact_old_kept'));
  }
  return lines;
}

/**
 * The update for one window: only the fields that differ from the topic, so
 * a save never rewrites what the window did not show.
 */
export function buildTopicUpdateRequest(instanceId, name, current, next) {
  const options = {};
  for (const [key, value] of Object.entries(next)) {
    if (value !== current[key]) options[key] = value;
  }
  return { instanceId, name, options };
}

/** The names that can change a topic, for "Zmiany w tym topiku może robić …". */
export function whoCanChange(adminLabels) {
  const names = (adminLabels || []).filter(Boolean);
  if (!names.length) return T('detail.who_can_instance');
  return T('detail.who_can_topic', { names: new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(names) });
}

// ---------------------------------------------------------------------------
// The section
// ---------------------------------------------------------------------------

function row(label, value, note, lock = false) {
  const noteHtml = note
    ? (lock ? `<div class="tb-vr-lock">${sprite('lock')}<span>${escapeHtml(note)}</span></div>` : `<div class="tb-vr-hint">${escapeHtml(note)}</div>`)
    : '';
  return `<div class="tb-vrow"><div class="tb-vr-label">${escapeHtml(label)}</div><div><div class="tb-vr-value">${escapeHtml(value)}</div>${noteHtml}</div></div>`;
}

function card(key, icon, title, rows, canAdmin) {
  return `
    <div class="section-card" data-card="${key}">
      <div class="section-card-head">
        <div class="title">${sprite(icon)} ${escapeHtml(title)}</div>
        ${canAdmin ? `<div class="actions"><tf-button variant="secondary" size="sm" icon="edit" data-go="change" data-card="${key}">${escapeHtml(T('settings.change'))}</tf-button></div>` : ''}
      </div>
      <div class="tb-vrows">${rows.join('')}</div>
    </div>`;
}

/** The subject bound to the topic, from the instance's pattern list. */
function boundSubject(topic, subjects) {
  return topic.schemaId ? (subjects || []).find((s) => s.subject === topic.schemaId) || null : null;
}

/** The whole section, as markup. `view` = the page's view (see topic-detail.js). */
export function settingsHtml(view) {
  const { topic, partitions, subjects, access, adminLabels, notice } = view;
  const canAdmin = Boolean(access?.canAdmin);
  const rf = Number(topic.replicationFactor) || 1;
  const facts = storageFacts(partitions);
  const subject = boundSubject(topic, subjects);

  const retention = [
    row(T('settings.retention.period_label'), fmtRetention(topic.retentionMs), T('settings.retention.period_hint', { minutes: fmtCount(SWEEP_MINUTES) })),
    row(T('settings.retention.limit_label'), fmtBytes(topic.retentionBytesPerPartition), facts.largest != null && facts.largest > 0
      ? T('settings.retention.limit_hint_now', { size: fmtBytes(facts.largest) })
      : T('settings.retention.limit_hint')),
    row(T('settings.retention.cleanup_label'), T('settings.retention.cleanup_value'), T('settings.retention.cleanup_lock'), true),
  ];
  const write = [
    row(T('settings.write.partitions_label'), fmtCount(topic.partitions), T('settings.write.partitions_hint')),
    row(T('settings.write.copies_label'), fmtCount(rf), T('settings.write.copies_lock'), true),
    row(T('settings.write.acks_label'), T(`settings.write.acks_${ACKS.includes(topic.acks) ? topic.acks : 'leader'}`), acksHint(topic.acks, rf)),
    row(T('settings.write.durability_label'), T(`settings.write.durability_${topic.durabilityClass === 'critical' ? 'critical' : 'standard'}`), T('settings.write.durability_lock'), true),
    row(T('settings.write.compression_label'), T(topic.compression === 'none' ? 'settings.write.compression_off' : 'settings.write.compression_on'),
      T(topic.compression === 'none' ? 'settings.write.compression_off_hint' : 'settings.write.compression_on_hint')),
  ];
  const retry = [
    row(T('settings.retry.attempts_label'), T('settings.retry.attempts_value', { count: fmtCount(topic.maxDeliveryAttempts), n: topic.maxDeliveryAttempts }), T('settings.retry.attempts_hint')),
    row(T('settings.retry.backoff_label'), fmtDuration(topic.retryBackoffMs), T('settings.retry.backoff_hint', { cap: fmtDuration(BACKOFF_CAP_MS) })),
  ];
  const pattern = [
    row(T('settings.pattern.kind_label'), contentTypeLabel(topic.contentType) || topic.contentType, T('settings.pattern.kind_lock'), true),
  ];
  if (topic.schemaId) {
    const format = subject ? schemaFormatLabel(subject.schemaType) : '';
    pattern.push(row(T('settings.pattern.schema_label'), format ? `${topic.schemaId} (${format})` : topic.schemaId,
      subject?.deprecatedAtMs != null
        ? T('settings.pattern.schema_hint_withdrawn')
        : (subject?.latestVersion ? T('settings.pattern.schema_hint', { version: fmtCount(subject.latestVersion) }) : '')));
    const mode = VALIDATION_MODES.includes(topic.validation) ? topic.validation : 'off';
    pattern.push(row(T('settings.pattern.mode_label'), T(`settings.pattern.mode_${mode}`), T(`settings.pattern.mode_${mode}_hint`)));
  } else {
    pattern.push(row(T('settings.pattern.schema_label'), T('settings.pattern.schema_none'), T('settings.pattern.schema_none_hint')));
  }

  return `
    ${notice ? `<tf-alert tone="${escapeAttr(notice.tone || 'success')}" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}" data-role="saved"></tf-alert>` : ''}
    ${access?.canRead ? '' : `<div class="tb-who-can">${sprite('lock')}<span>${escapeHtml(T('detail.no_read', { name: topic.name }))}</span></div>`}
    ${canAdmin ? '' : `<div class="tb-who-can">${sprite('lock')}<span>${escapeHtml(whoCanChange(adminLabels))}</span></div>`}
    ${card('retention', 'clock', T('settings.retention.title'), retention, canAdmin)}
    ${card('write', 'shield', T('settings.write.title'), write, canAdmin)}
    ${card('retry', 'rotate', T('settings.retry.title'), retry, canAdmin)}
    ${card('pattern', 'file-code', T('settings.pattern.title'), pattern, canAdmin)}
    ${canAdmin ? `
      <div class="tb-danger-zone">
        <h4>${sprite('alert')} ${escapeHtml(T('settings.delete.title'))}</h4>
        <div class="tb-dz-row">
          <div class="tb-dz-desc"><b>${escapeHtml(T('settings.delete.lead', { name: topic.name }))}</b> ${escapeHtml(T('settings.delete.text'))}</div>
          <tf-button variant="danger" icon="trash" data-go="delete">${escapeHtml(T('settings.delete.button'))}</tf-button>
        </div>
      </div>` : ''}`;
}

// ---------------------------------------------------------------------------
// The windows
// ---------------------------------------------------------------------------

/**
 * One "Zmień …" window. `fields()` = the body's controls (markup), `wire(win,
 * sync)` attaches their handlers and calls `sync()` on every change,
 * `draft()` = the values the controls hold now (or `null` when one is not
 * valid), `impact(draft)` = the "Co się stanie" sentences, `save(draft)`
 * sends it (may throw: the window stays with `describeError(err)`),
 * `onSaved(draft)` runs after the window closed.
 */
function openChangeWindow({ title, icon, width = 620, fields, wire, draft, current, impact, save, describeError, onSaved }) {
  const win = document.createElement('tf-window');
  win.className = 'tb-window tb-change-window';
  win.setAttribute('title', title);
  win.setAttribute('icon', icon);
  win.setAttribute('buttons', 'close');
  win.setAttribute('modal', '');
  win.setAttribute('draggable', '');
  win.setAttribute('width', String(width));
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      ${fields()}
      <div class="tb-will-happen" data-role="impact" aria-live="polite"></div>
      <div class="tb-window-error" role="alert" data-role="error" hidden>${sprite('alert')}<span></span></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-act="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-act="save" disabled>${escapeHtml(T('settings.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  let busy = false;
  const saveBtn = win.querySelector('[data-act="save"]');
  const cancelBtn = win.querySelector('[data-act="cancel"]');
  const changed = (d) => d != null && Object.keys(d).some((k) => d[k] !== current[k]);
  const sync = () => {
    const d = draft(win);
    const lines = d == null ? [] : impact(d);
    patchHtml(win.querySelector('[data-role="impact"]'), d == null
      ? `${sprite('info')}<div>${escapeHtml(T('settings.fix_fields'))}</div>`
      : changed(d)
        ? `${sprite('info')}<div><b>${escapeHtml(T('settings.will_happen'))}</b> ${lines.map(escapeHtml).join(' ')}</div>`
        : `${sprite('info')}<div>${escapeHtml(T('settings.nothing_changed'))}</div>`);
    saveBtn.toggleAttribute('disabled', busy || !changed(d));
    cancelBtn.toggleAttribute('disabled', busy);
  };
  wire(win, sync);
  sync();
  win.addEventListener('close-request', (e) => { if (busy) e.preventDefault(); });
  win.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.act === 'cancel') { win.close(true); return; }
    const d = draft(win);
    if (!changed(d)) return;
    busy = true;
    sync();
    const errEl = win.querySelector('[data-role="error"]');
    errEl.hidden = true;
    try {
      await save(d);
    } catch (err) {
      busy = false;
      sync();
      errEl.querySelector('span').textContent = describeError(err);
      errEl.hidden = false;
      return;
    }
    win.close(true);
    onSaved(d);
  });
  return win;
}

const selectMarkup = (id, label, hint) => `<tf-select id="${id}" label="${escapeAttr(label)}" hint="${escapeAttr(hint)}"></tf-select>`;

/**
 * Opens the window of one settings card. `ctx` = `{ instanceId, view,
 * update(request), describeError, onSaved(notice) }`; `view` is the page's
 * view (topic, partitions, subjects, capabilities).
 */
export function openSettingsWindow(cardKey, ctx) {
  const { view, instanceId } = ctx;
  const topic = view.topic;
  const titleOf = (key) => T(`settings.${key}.window_title`, { name: topic.name });
  const send = (current) => (next) => ctx.update(buildTopicUpdateRequest(instanceId, topic.name, current, next));
  const common = { describeError: ctx.describeError };

  if (cardKey === 'retention') {
    const current = { retentionMs: Number(topic.retentionMs), retentionBytesPerPartition: Number(topic.retentionBytesPerPartition) };
    const facts = storageFacts(view.partitions);
    return openChangeWindow({
      ...common,
      title: titleOf('retention'),
      icon: 'clock',
      current,
      fields: () => `
        ${selectMarkup('tb-set-retention', T('settings.retention.period_label'), T('settings.now', { value: fmtRetention(current.retentionMs) }))}
        ${selectMarkup('tb-set-limit', T('settings.retention.limit_label'), T('settings.now', { value: fmtBytes(current.retentionBytesPerPartition) }))}
        <div class="tb-vr-lock">${sprite('lock')}<span>${escapeHtml(T('settings.retention.window_cleanup_note'))}</span></div>`,
      wire: (win, sync) => {
        const r = win.querySelector('#tb-set-retention');
        r.setOptions(retentionChoices(current.retentionMs).map((ms) => ({ value: String(ms), label: fmtRetention(ms) })), String(current.retentionMs));
        const l = win.querySelector('#tb-set-limit');
        l.setOptions(limitChoices(current.retentionBytesPerPartition).map((b) => ({ value: String(b), label: fmtBytes(b) })), String(current.retentionBytesPerPartition));
        r.addEventListener('change', sync);
        l.addEventListener('change', sync);
      },
      draft: (win) => ({
        retentionMs: Number(win.querySelector('#tb-set-retention').value),
        retentionBytesPerPartition: Number(win.querySelector('#tb-set-limit').value),
      }),
      impact: (d) => retentionImpact({
        current: { retentionMs: current.retentionMs, limitBytes: current.retentionBytesPerPartition },
        next: { retentionMs: d.retentionMs, limitBytes: d.retentionBytesPerPartition },
        facts,
        nowMs: view.nowMs,
      }),
      save: send(current),
      onSaved: (d) => ctx.onSaved({
        card: 'retention',
        title: T('settings.retention.saved_title'),
        text: T('settings.retention.saved_text', { period: fmtRetention(d.retentionMs), limit: fmtBytes(d.retentionBytesPerPartition) }),
      }),
    });
  }

  if (cardKey === 'write') {
    const rf = Number(topic.replicationFactor) || 1;
    const current = { partitions: Number(topic.partitions), acks: topic.acks, compression: topic.compression === 'none' ? 'none' : 'lz4' };
    return openChangeWindow({
      ...common,
      title: titleOf('write'),
      icon: 'shield',
      width: 640,
      current,
      fields: () => `
        <tf-input id="tb-set-partitions" type="number" inputmode="numeric" stepper min="${current.partitions}" max="${PARTITIONS_MAX}" step="1"
          label="${escapeAttr(T('settings.write.partitions_label'))}" value="${current.partitions}" hint="${escapeAttr(T('settings.write.partitions_only_up', { count: fmtCount(current.partitions) }))}"
          stepper-dec-label="${escapeAttr(T('settings.write.partitions_less'))}" stepper-inc-label="${escapeAttr(T('settings.write.partitions_more'))}"></tf-input>
        ${selectMarkup('tb-set-acks', T('settings.write.acks_label'), T('settings.now', { value: T(`settings.write.acks_${ACKS.includes(current.acks) ? current.acks : 'leader'}`) }))}
        <div class="field">
          <label>${escapeHtml(T('settings.write.compression_label'))}</label>
          <tf-segmented id="tb-set-compression" size="md" value="${current.compression}"></tf-segmented>
        </div>
        <div class="tb-vr-lock">${sprite('lock')}<span>${escapeHtml(T('settings.write.window_durability_note'))}</span></div>`,
      wire: (win, sync) => {
        const p = win.querySelector('#tb-set-partitions');
        const onP = () => {
          const v = partitionCount(p.value, current.partitions);
          if (v == null) p.setAttribute('error', T('settings.write.partitions_invalid', { min: fmtCount(current.partitions), max: fmtCount(PARTITIONS_MAX) }));
          else p.removeAttribute('error');
          sync();
        };
        p.addEventListener('input', onP);
        p.addEventListener('change', onP);
        const a = win.querySelector('#tb-set-acks');
        a.setOptions(ACKS.map((v) => ({ value: v, label: T(`settings.write.acks_option_${v}`) })), ACKS.includes(current.acks) ? current.acks : 'leader');
        a.addEventListener('change', sync);
        const c = win.querySelector('#tb-set-compression');
        c.setOptions([{ value: 'lz4', label: T('settings.write.compression_on_option') }, { value: 'none', label: T('settings.write.compression_off_option') }], current.compression);
        c.addEventListener('change', sync);
      },
      draft: (win) => {
        const partitions = partitionCount(win.querySelector('#tb-set-partitions').value, current.partitions);
        if (partitions == null) return null;
        return { partitions, acks: win.querySelector('#tb-set-acks').value, compression: win.querySelector('#tb-set-compression').value };
      },
      impact: (d) => writeImpact({ current, next: d, rf }),
      save: send(current),
      onSaved: (d) => ctx.onSaved({
        card: 'write',
        title: T('settings.write.saved_title'),
        text: T('settings.write.saved_text', {
          count: fmtCount(d.partitions),
          n: d.partitions,
          when: T(`settings.write.acks_${d.acks}`),
          compression: T(d.compression === 'none' ? 'settings.write.compression_off' : 'settings.write.compression_on'),
        }),
      }),
    });
  }

  if (cardKey === 'retry') {
    const current = { maxDeliveryAttempts: Number(topic.maxDeliveryAttempts), retryBackoffMs: Number(topic.retryBackoffMs) };
    return openChangeWindow({
      ...common,
      title: titleOf('retry'),
      icon: 'rotate',
      current,
      fields: () => `
        <tf-input id="tb-set-attempts" type="number" inputmode="numeric" stepper min="${ATTEMPTS_MIN}" max="${ATTEMPTS_MAX}" step="1"
          label="${escapeAttr(T('settings.retry.attempts_label'))}" value="${current.maxDeliveryAttempts}" hint="${escapeAttr(T('settings.now', { value: T('settings.retry.attempts_value', { count: fmtCount(current.maxDeliveryAttempts), n: current.maxDeliveryAttempts }) }))}"
          stepper-dec-label="${escapeAttr(T('settings.retry.attempts_less'))}" stepper-inc-label="${escapeAttr(T('settings.retry.attempts_more'))}"></tf-input>
        ${selectMarkup('tb-set-backoff', T('settings.retry.backoff_label'), T('settings.retry.backoff_hint', { cap: fmtDuration(BACKOFF_CAP_MS) }))}`,
      wire: (win, sync) => {
        const a = win.querySelector('#tb-set-attempts');
        const onA = () => {
          if (attemptCount(a.value) == null) a.setAttribute('error', T('settings.retry.attempts_invalid', { min: fmtCount(ATTEMPTS_MIN), max: fmtCount(ATTEMPTS_MAX) }));
          else a.removeAttribute('error');
          sync();
        };
        a.addEventListener('input', onA);
        a.addEventListener('change', onA);
        const b = win.querySelector('#tb-set-backoff');
        b.setOptions(backoffChoices(current.retryBackoffMs).map((ms) => ({ value: String(ms), label: fmtDuration(ms) })), String(current.retryBackoffMs));
        b.addEventListener('change', sync);
      },
      draft: (win) => {
        const attempts = attemptCount(win.querySelector('#tb-set-attempts').value);
        if (attempts == null) return null;
        return { maxDeliveryAttempts: attempts, retryBackoffMs: Number(win.querySelector('#tb-set-backoff').value) };
      },
      impact: (d) => retryImpact({
        current: { attempts: current.maxDeliveryAttempts, backoffMs: current.retryBackoffMs },
        next: { attempts: d.maxDeliveryAttempts, backoffMs: d.retryBackoffMs },
      }),
      save: send(current),
      onSaved: (d) => ctx.onSaved({
        card: 'retry',
        title: T('settings.retry.saved_title'),
        text: T('settings.retry.saved_text', { count: fmtCount(d.maxDeliveryAttempts), n: d.maxDeliveryAttempts, pause: fmtDuration(d.retryBackoffMs) }),
      }),
    });
  }

  // cardKey === 'pattern'
  const subjects = view.subjects || [];
  const schemaTypes = view.capabilities?.schemaTypes || [];
  const bound = boundSubject(topic, subjects);
  const offered = compatibleSchemas(subjects, topic.contentType, schemaTypes);
  // The pattern the topic already checks with stays on the list even when it
  // could not be chosen anew (withdrawn): keeping it is not choosing it.
  if (bound && !offered.some((s) => s.subject === bound.subject)) offered.unshift(bound);
  const current = {
    schemaId: topic.schemaId || '',
    validation: topic.schemaId ? (VALIDATION_MODES.includes(topic.validation) ? topic.validation : 'off') : 'off',
  };
  const versionOf = (name) => Number(subjects.find((s) => s.subject === name)?.latestVersion) || 0;
  const kind = contentTypeLabel(topic.contentType) || topic.contentType;
  return openChangeWindow({
    ...common,
    title: titleOf('pattern'),
    icon: 'file-code',
    width: 640,
    current,
    fields: () => `
      ${offered.length ? '' : `<div class="tb-explain-box">${escapeHtml(T('settings.pattern.none_for_kind', { kind, formats: compatibleFormatsLabel(topic.contentType) }))}</div>`}
      ${selectMarkup('tb-set-schema', T('settings.pattern.schema_label'), offered.length
        ? T('settings.pattern.schema_choice_hint', { formats: compatibleFormatsLabel(topic.contentType), kind })
        : T('settings.pattern.schema_choice_none'))}
      <div data-role="mode-box">${selectMarkup('tb-set-mode', T('settings.pattern.mode_label'), T('settings.now', { value: current.schemaId ? T(`settings.pattern.mode_${current.validation}`) : T('settings.pattern.schema_none') }))}</div>`,
    wire: (win, sync) => {
      const s = win.querySelector('#tb-set-schema');
      s.setOptions([
        ...offered.map((x) => ({ value: x.subject, label: `${x.subject} · ${schemaFormatLabel(x.schemaType)}` })),
        { value: '', label: T('settings.pattern.schema_none') },
      ], current.schemaId);
      const m = win.querySelector('#tb-set-mode');
      m.setOptions(VALIDATION_MODES.map((v) => ({ value: v, label: T(`settings.pattern.mode_option_${v}`) })), current.schemaId ? current.validation : 'dlq');
      const box = win.querySelector('[data-role="mode-box"]');
      const onSchema = () => { box.hidden = !s.value; sync(); };
      s.addEventListener('change', onSchema);
      m.addEventListener('change', sync);
      box.hidden = !s.value;
    },
    draft: (win) => {
      const schemaId = win.querySelector('#tb-set-schema').value || '';
      return { schemaId, validation: schemaId ? win.querySelector('#tb-set-mode').value : 'off' };
    },
    impact: (d) => patternImpact({ current, next: d, versionOf }),
    // A cleared pattern is sent as an empty name: the server unbinds it and
    // stops checking in the same step.
    save: (d) => ctx.update({
      instanceId,
      name: topic.name,
      options: d.schemaId ? { schemaId: d.schemaId, validation: d.validation } : { schemaId: '' },
    }),
    onSaved: (d) => ctx.onSaved({
      card: 'pattern',
      title: T('settings.pattern.saved_title'),
      text: d.schemaId
        ? T(`settings.pattern.saved_text_${d.validation}`, { name: d.schemaId })
        : T('settings.pattern.saved_text_none'),
    }),
  });
}

/** A partition count typed in the field: a whole number from `min` to 256, else `null`. */
export function partitionCount(raw, min) {
  const text = String(raw ?? '').trim();
  if (!/^\d+$/.test(text)) return null;
  const n = Number(text);
  return n >= Number(min) && n <= PARTITIONS_MAX ? n : null;
}

/** An attempt count typed in the field: 1–100, else `null`. */
export function attemptCount(raw) {
  const text = String(raw ?? '').trim();
  if (!/^\d+$/.test(text)) return null;
  const n = Number(text);
  return n >= ATTEMPTS_MIN && n <= ATTEMPTS_MAX ? n : null;
}
