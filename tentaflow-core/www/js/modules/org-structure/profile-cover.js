// =============================================================================
// File: modules/org-structure/profile-cover.js
// Description: The sections of the profile page that belong to the org
//   structure (mockup g01, "Nieobecności i zastępstwa"): the person's absences
//   (add, change, delete — their own, entered by hand), who covers them and
//   whom they cover. Everything is read through `orgCoverRequest`, which the
//   server already filtered by privacy; an absence has no reason to show.
//   Deputies are set by administrators (docs §2.2): the button is drawn only
//   when the answer says the caller may, and a person without that right gets
//   a line saying who sets them.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-avatar.js';
import { openActionMenu } from '/js/lib/actions/index.js';
import { formatDay } from '/js/lib/date-format.js';
import { focusTarget } from '/js/lib/actions/fields.js';
import {
  accountPeople, createCoverActions, rangeLabel, scopeLabel,
} from '/js/modules/org-structure/cover-actions.js';
import {
  absenceRows, dayCount, deputyRows, initials,
} from '/js/modules/org-structure/cover-model.js';
import { absenceRecords } from '/js/modules/org-structure/handover-model.js';
import { openHandover } from '/js/modules/org-structure/handover-nav.js';

const ct = (key, params) => I18n.t(`org_structure.cover.${key}`, params);
const hv = (key, params) => I18n.t(`org_structure.handover.${key}`, params);

const PHASE_STATUS = { current: 'warn', upcoming: 'info', past: 'neutral' };

function phaseChip(phase) {
  return `<tf-chip status="${PHASE_STATUS[phase]}">${escapeHtml(ct(`phase_${phase}`))}</tf-chip>`;
}

function absenceRowHtml({ absence, phase, manual }, canEdit) {
  const days = dayCount(absence);
  const kind = ct(`kind_${absence.kind}`);
  const meta = [kind, days == null ? ct('open_ended') : ct('days', { count: days })];
  const source = manual ? ct('source_manual') : absence.source;
  return `
    <div class="org-cover-row" data-kind="absence" data-id="${escapeAttr(absence.id)}">
      <div class="org-cover-main">
        <div class="org-cover-title">${escapeHtml(rangeLabel(absence))}</div>
        <div class="org-cover-meta">${escapeHtml(meta.join(' · '))}</div>
      </div>
      <div class="org-cover-tags">${phaseChip(phase)}<tf-chip status="neutral">${escapeHtml(source)}</tf-chip></div>
      ${canEdit ? `<tf-button class="org-cover-more" variant="ghost" size="sm" icon="more" data-act="absence-menu"
        aria-label="${escapeAttr(ct('more'))}" title="${escapeAttr(ct('more'))}"></tf-button>` : '<span class="org-cover-more"></span>'}
    </div>`;
}

function deputyRowHtml({ deputy, phase }, side, canEdit) {
  const name = side === 'covering' ? deputy.user_name : deputy.deputy_name;
  return `
    <div class="org-cover-row" data-kind="deputy" data-id="${escapeAttr(deputy.id)}">
      <tf-avatar initials="${escapeAttr(initials(name))}" size="sm"></tf-avatar>
      <div class="org-cover-main">
        <div class="org-cover-title">${escapeHtml(name || '—')}</div>
        <div class="org-cover-meta">${escapeHtml(`${scopeLabel(deputy.scope)} · ${rangeLabel(deputy)}`)}</div>
      </div>
      <div class="org-cover-tags">${phaseChip(phase)}</div>
      ${canEdit ? `<tf-button class="org-cover-more" variant="ghost" size="sm" icon="more" data-act="deputy-menu"
        aria-label="${escapeAttr(ct('more'))}" title="${escapeAttr(ct('more'))}"></tf-button>` : '<span class="org-cover-more"></span>'}
    </div>`;
}

function handoverRowHtml({ record, away, done, returned, kept, failed }) {
  const parts = [hv('profile_done', { count: done + returned + kept })];
  if (returned) parts.push(hv('profile_returned', { count: returned }));
  if (kept) parts.push(hv('profile_kept', { count: kept }));
  if (failed) parts.push(hv('profile_failed', { count: failed }));
  return `
    <div class="org-cover-row" data-kind="handover" data-id="${escapeAttr(record.id)}">
      <div class="org-cover-main">
        <div class="org-cover-title">${escapeHtml(hv('profile_until', { date: formatDay(record.return_date) }))}</div>
        <div class="org-cover-meta">${escapeHtml(parts.join(' · '))}</div>
        <div class="org-cover-meta">${escapeHtml(record.note)}</div>
      </div>
      <div class="org-cover-tags">${phaseChip(away ? 'current' : 'past')}</div>
      <span class="org-cover-more"></span>
    </div>`;
}

function section(id, title, body, actions = '') {
  return `
    <section class="org-cover-section" id="${id}" aria-labelledby="${id}-title">
      <div class="org-cover-head"><h2 id="${id}-title">${escapeHtml(title)}</h2>${actions}</div>
      ${body}
    </section>`;
}

function empty(text) {
  return `<div class="org-cover-empty">${escapeHtml(text)}</div>`;
}

/**
 * Draws the org sections of the profile into `host` and keeps them current after every write.
 * Answers a dispose function; a person who is not a member of any organization gets nothing.
 */
export async function mountProfileCover(host) {
  let cover = null;
  let handovers = [];
  let disposed = false;
  const actions = createCoverActions({ reload, people: accountPeople() });

  async function load() {
    cover = await ApiBinary.one('orgCoverRequest', { includePast: true });
    // The record of what was handed over is a convenience: the sections above do not wait on it.
    handovers = (await ApiBinary.one('orgHandoverRecordsRequest', {}).catch(() => null))?.records ?? [];
  }

  function render() {
    if (disposed || !cover) return;
    const today = cover.today;
    const absences = absenceRows(cover.absences, today);
    const isAdmin = Boolean(cover.is_admin);
    const canEditDeputies = Boolean(cover.can_edit_deputies);
    const canEditAbsence = (row) => Boolean(cover.can_edit_absences) && (row.manual || isAdmin);
    const add = cover.can_edit_absences
      ? `<tf-button size="sm" icon="plus" data-act="absence-add">${escapeHtml(ct('absence_add'))}</tf-button>` : '';
    const absenceBody = `
      <p class="org-cover-note">${escapeHtml(ct('absence_privacy_note'))}</p>
      ${absences.length ? absences.map((row) => absenceRowHtml(row, canEditAbsence(row))).join('') : empty(ct('absence_empty'))}`;

    const covered = deputyRows(cover.covered_by, today);
    const covering = deputyRows(cover.covering, today);
    const addDeputy = canEditDeputies
      ? `<tf-button size="sm" icon="plus" data-act="deputy-add">${escapeHtml(ct('deputy_add'))}</tf-button>` : '';
    const coveredBody = `
      ${covered.length ? covered.map((row) => deputyRowHtml(row, 'covered_by', canEditDeputies)).join('') : empty(ct('covered_by_empty'))}`;

    const records = absenceRecords(handovers, today);
    const handoverBody = `
      <p class="org-cover-note">${escapeHtml(hv('profile_note'))}</p>
      ${records.length ? records.map(handoverRowHtml).join('') : empty(hv('profile_empty'))}`;
    const handoverButton = `<tf-button size="sm" variant="secondary" icon="send" data-act="handover-open">${escapeHtml(hv('profile_open'))}</tf-button>`;

    host.innerHTML = `
      <div class="org-cover">
        ${section('org-cover-absences', ct('absences_title'), absenceBody, add)}
        ${section('org-cover-handover', hv('profile_title'), handoverBody, handoverButton)}
        ${section('org-cover-covered-by', ct('covered_by_title'), coveredBody, addDeputy)}
        ${section('org-cover-covering', ct('covering_title'), covering.length
    ? covering.map((row) => deputyRowHtml(row, 'covering', isAdmin)).join('') : empty(ct('covering_empty')))}
      </div>`;
  }

  async function reload() {
    await load();
    render();
  }

  const findAbsence = (id) => cover?.absences.find((a) => a.id === id);
  const findDeputy = (id) => [...cover.covered_by, ...cover.covering].find((d) => d.id === id);

  function onClick(e) {
    const button = e.target.closest('[data-act]');
    if (!button || !cover || button.hasAttribute('disabled')) return;
    const rowId = button.closest('[data-id]')?.dataset.id;
    // Rows are drawn again after every write, so the button a window was opened from may be gone when it
    // closes: focus goes back to the same control by its role and row, wherever it is now.
    const act = button.dataset.act;
    const anchor = {
      isConnected: true,
      focus() {
        const scope = rowId ? host.querySelector(`[data-id="${rowId.replace(/["\\]/g, '')}"]`) : host;
        focusTarget(scope?.querySelector(`[data-act="${act}"]`) ?? host.querySelector('[data-act]'))?.focus?.();
      },
    };
    switch (button.dataset.act) {
      case 'absence-add':
        actions.addAbsence({ subject: cover.display_name, today: cover.today, anchor });
        break;
      case 'absence-menu': {
        const absence = findAbsence(rowId);
        if (!absence) break;
        const subject = `${rangeLabel(absence)} — ${ct(`kind_${absence.kind}`)}`;
        openActionMenu(button, [
          { label: ct('menu_edit'), icon: 'edit', run: () => actions.editAbsence(absence, { subject, anchor }) },
          { separator: true },
          { label: ct('menu_delete'), icon: 'trash', danger: true, run: () => actions.deleteAbsence(absence, { subject, anchor }) },
        ], subject);
        break;
      }
      case 'handover-open':
        openHandover({ userId: cover.user_id, reason: 'absence' });
        break;
      case 'deputy-add':
        actions.addDeputy({ userId: cover.user_id, userName: cover.display_name, today: cover.today, anchor });
        break;
      case 'deputy-menu': {
        const deputy = findDeputy(rowId);
        if (!deputy) break;
        const subject = ct('deputy_subject', { deputy: deputy.deputy_name, person: deputy.user_name });
        openActionMenu(button, [
          { label: ct('menu_edit'), icon: 'edit', run: () => actions.editDeputy(deputy, { anchor }) },
          { separator: true },
          { label: ct('menu_end'), icon: 'trash', danger: true, run: () => actions.endDeputy(deputy, { today: cover.today, anchor }) },
        ], subject);
        break;
      }
      default:
        break;
    }
  }

  try {
    await load();
  } catch {
    // Not a member of an organization, or the structure is unreachable: the profile keeps its own card.
    host.replaceChildren();
    return () => {};
  }
  host.addEventListener('click', onClick);
  render();
  return () => {
    disposed = true;
    host.removeEventListener('click', onClick);
    host.replaceChildren();
  };
}
