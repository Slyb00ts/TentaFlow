// ===== File: modules/tentaquant/settings.js — Q12, the Ustawienia tab of one laboratory =====
//
// Three cards, each backed by something Core stores and enforces today:
//
//   * Kurs     the course ranking switch — `quant.instruct` decides it (§10.2).
//   * Limity   the browser and Core qubit ceilings and how many T1 runs the
//              laboratory executes at once; every member reads them, only
//              `quant.admin` changes them. These are the numbers `Target::List`
//              and the run slots actually apply.
//   * Dostęp   who the matrix admits and with which permissions. Read-only on
//              purpose: the matrix is edited in Addons, and a second editor here
//              would be a second source of truth.
//
// What the mockup shows and this does NOT build: IBM accounts, the QPU pool,
// per-person limits, node isolation and retention. They have no backend, and
// the stored fields that belong to tiers this build does not have (Python and
// GPU ceilings, kernel timeouts, default tier) are not shown as settings that
// would change nothing.
//
// The server stores ONE settings document and rejects a partial one, so every
// save sends the whole document it last read with just the card's own fields
// replaced (`buildSettings`). The fields no card shows travel back untouched.

import { I18n } from '/js/i18n.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import {
  T, sprite, errMessage, has, initials, permissionSummary, roleLabel, roleOf,
} from '/js/modules/tentaquant/format.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-input.js';
import '/js/components/tf-toggle.js';

/// The limits a card edits, with the bounds the server validates them against
/// (`validate_settings`). The server stays the authority — Core's own ceiling is
/// lower than 40 and it says so in the refusal — these only keep the control
/// from offering nonsense.
export const LIMIT_FIELDS = [
  { key: 'maxQubitsBrowser', min: 1, max: 40 },
  { key: 'maxQubitsCore', min: 1, max: 40 },
  { key: 'maxConcurrentCoreRuns', min: 1, max: 32 },
];

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Who may do what on this tab, from the caller's permissions. The tab itself
/// is shown to a supervisor or an admin; an admin without `quant.instruct`
/// still edits limits but cannot list people (`Lab::People` is a supervisor's
/// request).
export function settingsAccess(permissions) {
  return {
    canOpen: has(permissions, 'quant.instruct') || has(permissions, 'quant.admin'),
    canRanking: has(permissions, 'quant.instruct') || has(permissions, 'quant.admin'),
    canLimits: has(permissions, 'quant.admin'),
    canPeople: has(permissions, 'quant.instruct'),
  };
}

/// The whole document to send: what the server last answered, with the fields
/// of one card replaced. A card never has to know the fields it does not show.
export function buildSettings(current, patch) {
  return { ...current, ...patch };
}

/// One typed limit as a whole number inside its bounds, or null when it is not
/// one — the Save button stays down on null rather than the server answering a
/// refusal for a half-typed value.
export function parseLimit(field, text) {
  const raw = String(text ?? '').trim();
  if (!/^\d+$/.test(raw)) return null;
  const n = Number(raw);
  return n >= field.min && n <= field.max ? n : null;
}

/// The limits the inputs hold, or null if any one is not valid.
export function readLimits(values) {
  const out = {};
  for (const field of LIMIT_FIELDS) {
    const n = parseLimit(field, values[field.key]);
    if (n === null) return null;
    out[field.key] = n;
  }
  return out;
}

/// Whether the limits differ from what is stored.
export function limitsChanged(current, limits) {
  return Boolean(limits) && LIMIT_FIELDS.some((f) => Number(current[f.key]) !== limits[f.key]);
}

/// People of the matrix, the most privileged first and then by name, so the
/// supervisors a person would look for are at the top.
export function sortPeople(people) {
  const rank = { admin: 0, supervisor: 1, user: 2, observer: 3 };
  return (people || []).slice().sort((a, b) => rank[roleOf(a.permissions)] - rank[roleOf(b.permissions)]
    || String(a.displayName || '').localeCompare(String(b.displayName || '')));
}

/// How many people hold each role, for the summary line.
export function roleCounts(people) {
  const counts = { admin: 0, supervisor: 0, user: 0, observer: 0 };
  for (const person of people || []) counts[roleOf(person.permissions)] += 1;
  return counts;
}

// ---------------------------------------------------------------------------
// Markup
// ---------------------------------------------------------------------------

function cardHead(icon, titleKey, hintKey) {
  return `
    <div class="section-card-head">
      <div class="title">${sprite(icon)} ${escapeHtml(T(titleKey))}</div>
      ${hintKey ? `<span class="hint">${escapeHtml(T(hintKey))}</span>` : ''}
    </div>`;
}

function rankingCard(settings, access) {
  return `
    <div class="section-card" id="tq-set-ranking">
      ${cardHead('crown', 'settings.ranking_title')}
      <label class="toggle-row">
        <tf-toggle id="tq-set-ranking-toggle" ${settings.rankingEnabled ? 'checked' : ''} ${access.canRanking ? '' : 'disabled'}></tf-toggle>
        <span><span class="tr-name">${escapeHtml(T('settings.ranking_name'))}</span><span class="tr-sub">${escapeHtml(T('settings.ranking_sub'))}</span></span>
      </label>
      <div class="kata-actions">
        <tf-button variant="primary" icon="save" data-act="save-ranking" disabled>${escapeHtml(T('settings.save'))}</tf-button>
      </div>
    </div>`;
}

function limitsCard(settings, access) {
  const field = (f) => `
    <tf-input id="tq-set-${f.key}" type="number" stepper min="${f.min}" max="${f.max}" step="1"
      label="${escapeAttr(T(`settings.limit_${f.key}`))}" hint="${escapeAttr(T(`settings.limit_${f.key}_hint`))}"
      value="${Number(settings[f.key]) || ''}" ${access.canLimits ? '' : 'disabled'}
      stepper-dec-label="${escapeAttr(T('settings.limit_dec'))}" stepper-inc-label="${escapeAttr(T('settings.limit_inc'))}"></tf-input>`;
  return `
    <div class="section-card" id="tq-set-limits">
      ${cardHead('gauge', 'settings.limits_title')}
      <div class="section-sub">${escapeHtml(T('settings.limits_sub'))}</div>
      <div class="set-grid">${LIMIT_FIELDS.map(field).join('')}</div>
      <div class="kata-actions">
        ${access.canLimits
          ? `<tf-button variant="primary" icon="save" data-act="save-limits" disabled>${escapeHtml(T('settings.save'))}</tf-button>`
          : `<span class="hint">${escapeHtml(T('settings.limits_admin_only'))}</span>`}
      </div>
      <div id="tq-set-limits-error" class="tq-form-error" hidden></div>
    </div>`;
}

function accessCard(lab, people, peopleError) {
  const head = `
    <div class="section-card-head">
      <div class="title">${sprite('shield')} ${escapeHtml(T('settings.access_title'))}</div>
      <div class="actions"><tf-button variant="secondary" size="sm" icon="external-link" data-act="addons">${escapeHtml(T('settings.access_edit'))}</tf-button></div>
    </div>
    <div class="section-sub">${escapeHtml(T('settings.access_sub'))}</div>`;
  const mine = `<div class="kv set-mine">
      <span class="k">${escapeHtml(T('settings.access_you'))}</span>
      <span class="v">${escapeHtml(roleLabel(lab?.myPermissions))} <span class="mono text-3">${escapeHtml(permissionSummary(lab?.myPermissions))}</span></span>
    </div>`;
  if (peopleError) {
    return `<div class="section-card" id="tq-set-access">${head}${mine}<tf-alert tone="danger" title="${escapeAttr(T('settings.people_failed'))}" message="${escapeAttr(peopleError)}"></tf-alert></div>`;
  }
  if (!people) {
    return `<div class="section-card" id="tq-set-access">${head}${mine}<div class="hint">${escapeHtml(T('settings.people_supervisor_only'))}</div></div>`;
  }
  const counts = roleCounts(people);
  const rows = sortPeople(people).map((person) => `
    <tr>
      <td><span class="member-cell"><span class="member-avatar">${escapeHtml(initials(person.displayName))}</span><span class="member-name">${escapeHtml(person.displayName || person.userId)}</span></span></td>
      <td><tf-chip status="${roleOf(person.permissions) === 'observer' ? 'neutral' : 'info'}" label="${escapeAttr(roleLabel(person.permissions))}"></tf-chip></td>
      <td class="mono text-3">${escapeHtml(permissionSummary(person.permissions))}</td>
    </tr>`).join('');
  return `
    <div class="section-card" id="tq-set-access">
      ${head}${mine}
      ${people.length === 0
        ? `<tf-empty-state icon="users" title="${escapeAttr(T('settings.people_empty'))}"></tf-empty-state>`
        : `<div class="table-scroll"><table class="tf-table set-people">
            <thead><tr><th>${escapeHtml(T('settings.col_person'))}</th><th>${escapeHtml(T('settings.col_role'))}</th><th>${escapeHtml(T('settings.col_permissions'))}</th></tr></thead>
            <tbody>${rows}</tbody>
          </table></div>
          <div class="tq-table-footer"><span>${escapeHtml(T('settings.people_footer', { n: people.length }))}</span>
            <span>${escapeHtml(T('settings.people_roles', counts))}</span></div>`}
    </div>`;
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// Loads the settings (and, for a supervisor, the people) and draws the tab
/// into `host`.
export async function drawSettings(screen, host) {
  const draw = (screen.settingsDraw = (screen.settingsDraw || 0) + 1);
  const instanceId = screen.instanceId;
  const stale = () => screen.disposed || screen.settingsDraw !== draw
    || screen.instanceId !== instanceId || screen.tab !== 'settings' || !host.isConnected;
  const access = settingsAccess(screen.lab?.myPermissions);

  host.innerHTML = `<div class="tq-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
  let current;
  try {
    current = (await screen.tq('tentaQuantSettingsGetRequest')).settings;
  } catch (e) {
    if (stale()) return;
    host.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('settings.load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
    return;
  }
  let people = null;
  let peopleError = '';
  if (access.canPeople) {
    try {
      people = (await screen.tq('tentaQuantLabPeopleRequest')).people || [];
    } catch (e) {
      peopleError = errMessage(e);
    }
  }
  if (stale()) return;

  host.innerHTML = `
    ${rankingCard(current, access)}
    ${limitsCard(current, access)}
    ${accessCard(screen.lab, people, peopleError)}`;

  const save = async (button, patch) => {
    button.setAttribute('disabled', '');
    try {
      await screen.tq('tentaQuantSettingsSetRequest', { settings: buildSettings(current, patch) });
      toast(T('settings.saved'), 'success');
    } catch (e) {
      if (!stale()) button.removeAttribute('disabled');
      return errMessage(e);
    }
    if (!stale()) await drawSettings(screen, host);
    return '';
  };

  const toggle = host.querySelector('#tq-set-ranking-toggle');
  const saveRanking = host.querySelector('[data-act="save-ranking"]');
  toggle.addEventListener('change', (e) => {
    const wanted = Boolean(e.detail?.checked);
    if (wanted === Boolean(current.rankingEnabled)) saveRanking.setAttribute('disabled', '');
    else saveRanking.removeAttribute('disabled');
  });
  saveRanking.addEventListener('click', async () => {
    const error = await save(saveRanking, { rankingEnabled: toggle.checked });
    if (error) toast(`${T('settings.save_failed')}: ${error}`, 'error');
  });

  const saveLimits = host.querySelector('[data-act="save-limits"]');
  if (saveLimits) {
    const inputs = LIMIT_FIELDS.map((f) => host.querySelector(`#tq-set-${f.key}`));
    const typed = () => Object.fromEntries(LIMIT_FIELDS.map((f, i) => [f.key, inputs[i].value]));
    const refresh = () => {
      const limits = readLimits(typed());
      if (limitsChanged(current, limits)) saveLimits.removeAttribute('disabled');
      else saveLimits.setAttribute('disabled', '');
    };
    inputs.forEach((input) => input.addEventListener('input', refresh));
    saveLimits.addEventListener('click', async () => {
      const limits = readLimits(typed());
      if (!limits) return;
      const error = await save(saveLimits, limits);
      const slot = host.querySelector('#tq-set-limits-error');
      if (error && slot) { slot.textContent = error; slot.hidden = false; }
    });
  }

  host.querySelector('[data-act="addons"]').addEventListener('click', () => screen.openAddons());
}
