// ===== File: modules/tentanas/schedule-editor.js — the NasSchedule form (cadence + time) and the single-schedule editor window used by scrub, snapshot and SMART schedules =====
//
// One schedule shape drives every recurring task of the node
// (`{ every, hour, minute, weekday, day }`), so the fields are rendered by one
// function and read back by one function. The dialogs that need more than a
// cadence (GFS retention for snapshots, two cadences for SMART) embed these
// fields and add their own rows around them.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, fmtSchedule } from '/js/modules/tentanas/format.js';
import '/js/components/tf-window.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
import '/js/components/tf-toggle.js';

export const EVERY_OPTIONS = ['15m', '30m', '1h', '6h', 'daily', 'weekly', 'monthly'];

// Cadences shorter than a day carry no clock time; the time/weekday/day rows
// are hidden instead of disabled so the form only shows what applies.
const hasTime = (every) => every === 'daily' || every === 'weekly' || every === 'monthly';

export function normalizeSchedule(s) {
  const src = s || {};
  const every = EVERY_OPTIONS.includes(src.every) ? src.every : 'daily';
  return {
    every,
    hour: clamp(src.hour, 0, 23, 0),
    minute: clamp(src.minute, 0, 59, 0),
    weekday: clamp(src.weekday, 0, 6, 0),
    day: clamp(src.day, 1, 28, 1),
  };
}

function clamp(v, min, max, dflt) {
  const n = Number(v);
  if (!Number.isFinite(n)) return dflt;
  return Math.min(max, Math.max(min, Math.round(n)));
}

/**
 * Markup of the schedule fields. `prefix` namespaces the element ids so two
 * schedules (SMART short + long) can sit in one window.
 */
export function scheduleFieldsHtml(prefix, schedule, { allowed = EVERY_OPTIONS } = {}) {
  const s = normalizeSchedule(schedule);
  const showTime = hasTime(s.every);
  return `
    <div class="sched-fields" data-sched="${escapeAttr(prefix)}" data-allowed="${escapeAttr(allowed.join(','))}">
      <tf-select id="${escapeAttr(prefix)}-every" label="${escapeAttr(T('schedule.every_label'))}"></tf-select>
      <div class="form-grid-2" data-sched-part="time" ${showTime ? '' : 'hidden'}>
        <tf-input id="${escapeAttr(prefix)}-hour" type="number" min="0" max="23" step="1" inputmode="numeric" label="${escapeAttr(T('schedule.hour'))}" value="${s.hour}"></tf-input>
        <tf-input id="${escapeAttr(prefix)}-minute" type="number" min="0" max="59" step="1" inputmode="numeric" label="${escapeAttr(T('schedule.minute'))}" value="${s.minute}"></tf-input>
      </div>
      <div data-sched-part="weekday" ${s.every === 'weekly' ? '' : 'hidden'}>
        <tf-select id="${escapeAttr(prefix)}-weekday" label="${escapeAttr(T('schedule.weekday'))}"></tf-select>
      </div>
      <div data-sched-part="day" ${s.every === 'monthly' ? '' : 'hidden'}>
        <tf-input id="${escapeAttr(prefix)}-day" type="number" min="1" max="28" step="1" inputmode="numeric" label="${escapeAttr(T('schedule.day'))}" hint="${escapeAttr(T('schedule.day_hint'))}" value="${s.day}"></tf-input>
      </div>
      <div class="muted" data-sched-part="preview">${escapeHtml(fmtSchedule(s))}</div>
    </div>`;
}

/** Fills the selects and keeps the time rows in sync with the cadence. */
export function wireScheduleFields(root, prefix, schedule, onChange = null) {
  const s = normalizeSchedule(schedule);
  const box = root.querySelector(`[data-sched="${prefix}"]`);
  if (!box) return;
  const allowed = (box.dataset.allowed || '').split(',').filter(Boolean);
  const every = box.querySelector(`#${prefix}-every`);
  every.setOptions(allowed.map((v) => ({ value: v, label: T('schedule.every_' + v) })), s.every);
  const weekday = box.querySelector(`#${prefix}-weekday`);
  weekday.setOptions([0, 1, 2, 3, 4, 5, 6].map((d) => ({ value: String(d), label: T('weekday.' + d) })), String(s.weekday));
  const sync = () => {
    const cur = readScheduleFields(root, prefix);
    box.querySelector('[data-sched-part="time"]').hidden = !hasTime(cur.every);
    box.querySelector('[data-sched-part="weekday"]').hidden = cur.every !== 'weekly';
    box.querySelector('[data-sched-part="day"]').hidden = cur.every !== 'monthly';
    box.querySelector('[data-sched-part="preview"]').textContent = fmtSchedule(cur);
    if (onChange) onChange(cur);
  };
  every.addEventListener('change', sync);
  weekday.addEventListener('change', sync);
  for (const id of ['hour', 'minute', 'day']) {
    const el = box.querySelector(`#${prefix}-${id}`);
    el.addEventListener('input', sync);
    el.addEventListener('change', sync);
  }
}

/** Reads the fields back as a NasSchedule (always every field, clamped). */
export function readScheduleFields(root, prefix) {
  const box = root.querySelector(`[data-sched="${prefix}"]`);
  const val = (id) => box.querySelector(`#${prefix}-${id}`)?.value;
  return normalizeSchedule({
    every: val('every'),
    hour: val('hour'),
    minute: val('minute'),
    weekday: val('weekday'),
    day: val('day'),
  });
}

/**
 * Editor window for one schedule with an enabled switch (scrub of a pool,
 * n06/n15). `onSave({ enabled, schedule })` runs on confirm and may throw —
 * the window then stays open with the error on the button row.
 */
export function openScheduleEditor({ title, subtitle = '', icon = 'clock', schedule, enabled = true, allowed = EVERY_OPTIONS, note = '', onSave }) {
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', title);
  if (subtitle) win.setAttribute('subtitle', subtitle);
  win.setAttribute('icon', icon);
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '560');
  win.setAttribute('min-width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('schedule.enabled'))}</span><span class="tc-sub">${escapeHtml(T('schedule.enabled_sub'))}</span></div>
        <tf-toggle id="nas-sched-enabled" ${enabled ? 'checked' : ''}></tf-toggle>
      </div>
      ${scheduleFieldsHtml('nas-sched', schedule, { allowed })}
      ${note ? `<div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(note)}</div></div>` : ''}
      <div class="num-err" id="nas-sched-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(T('schedule.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  wireScheduleFields(win, 'nas-sched', schedule);
  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    busy = true;
    const btn = win.querySelector('[data-action="confirm"]');
    btn.setAttribute('disabled', '');
    try {
      await onSave({ enabled: Boolean(win.querySelector('#nas-sched-enabled').checked), schedule: readScheduleFields(win, 'nas-sched') });
      win.close(true);
    } catch (err) {
      busy = false;
      btn.removeAttribute('disabled');
      const errEl = win.querySelector('#nas-sched-error');
      errEl.textContent = err && err.message ? err.message : String(err);
      errEl.hidden = false;
    }
  });
  return win;
}
