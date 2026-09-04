// ===== File: modules/tentaquant/format.js — labels, permissions and timestamps shared by the TentaQuant screen and its lab/project views =====
//
// Everything the views render goes through the same helpers so a role, a
// people count or a timestamp reads identically on the laboratory tiles, the
// lab dashboard and the project cards. The i18n namespace is fixed here
// (`tentaquant.*`) so no view repeats the prefix.
//
// The pure functions below (role resolution, sectioning, the one-lab entry
// rule) hold the screen's decisions and are unit-tested without a DOM.

import { I18n } from '/js/i18n.js';

export const T = (k, p) => I18n.t('tentaquant.' + k, p);
export const sprite = (id) => `<svg class="icon"><use href="#i-${id}"/></svg>`;

// The six permission ids of the app manifest (plan §10.2), in the order the
// permission line shows them.
const PERMISSIONS = [
  'quant.read',
  'quant.run',
  'quant.run.gpu',
  'quant.run.qpu',
  'quant.instruct',
  'quant.admin',
];

export function has(permissions, id) {
  return Array.isArray(permissions) && permissions.includes(id);
}

// A laboratory has no owner (§18 decision 26): what a tile shows instead is the
// role the instance matrix resolves the caller to. The strongest permission
// names the role, which is exactly how the plan defines the three sets
// (observer = read, user = + run, supervisor = + instruct).
export function roleOf(permissions) {
  if (has(permissions, 'quant.admin')) return 'admin';
  if (has(permissions, 'quant.instruct')) return 'supervisor';
  if (has(permissions, 'quant.run')) return 'user';
  return 'observer';
}

export function roleLabel(permissions) {
  return T('labs.role_' + roleOf(permissions));
}

// The granted permissions, shortened the way the mockup writes them
// ("read · run · run.gpu"), in manifest order so two labs compare by eye.
export function permissionSummary(permissions) {
  return PERMISSIONS.filter((p) => has(permissions, p))
    .map((p) => p.replace(/^quant\./, ''))
    .join(' · ');
}

// A node the instance reconciled onto. `instance_status` is the platform's
// stored row, not a live probe, so "offline" (the node) and "not ready" (the
// instance on it) are different facts and both are shown.
export function nodeState(node) {
  if (!node) return 'unknown';
  if (!node.online) return 'offline';
  return node.instanceStatus === 'ready' ? 'ready' : 'not_ready';
}

export function nodeStateLabel(node) {
  return T('labs.node_' + nodeState(node));
}

export function labIsReady(lab) {
  return (lab?.nodes || []).some((n) => nodeState(n) === 'ready');
}

// A one-person laboratory is somebody's own sandbox and says so instead of
// counting to one ("tylko Ty" in the mockup).
export function isSolo(lab) {
  return Number(lab?.peopleCount ?? 0) <= 1;
}

// Which laboratory the `#/tentaquant` route opens (plan §19.8): the list when
// there is a choice to make, straight into the lab when there is not. An
// explicit `?instance=` always wins — that is what an apps-home tile and a
// bookmark carry — and an instance that is not in the list (uninstalled,
// access revoked) falls back to the list rather than to an empty screen.
export function chooseEntryLab(labs, requestedInstanceId) {
  const list = Array.isArray(labs) ? labs : [];
  if (requestedInstanceId) {
    const named = list.find((l) => l.instanceId === requestedInstanceId);
    if (named) return named.instanceId;
    return null;
  }
  const enterable = list.filter((l) => l.enabled);
  return enterable.length === 1 ? enterable[0].instanceId : null;
}

// Q03 sections. The wire carries the resolved role and the visibility, and the
// pair decides the section: what I own, what somebody handed me by name, and
// what the whole laboratory reads. A project published to the lab reaches every
// member as `viewer`, so an explicit editor share on it still counts as shared
// with me — the stronger role is the reason I see more than the others do.
export function sectionOf(project) {
  if (project.myRole === 'owner') return 'mine';
  if (project.visibility === 'lab' && project.myRole === 'viewer') return 'lab';
  return 'shared';
}

export function sectionProjects(projects) {
  const out = { mine: [], shared: [], lab: [] };
  for (const p of Array.isArray(projects) ? projects : []) out[sectionOf(p)].push(p);
  return out;
}

// Core timestamps are naive UTC "YYYY-MM-DD HH:MM:SS"; RFC3339 passes through.
function parseServerTs(s) {
  if (!s) return null;
  const str = String(s);
  const iso = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}/.test(str) ? str.replace(' ', 'T') + 'Z' : str;
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? null : d;
}

export function fmtDate(s) {
  const d = parseServerTs(s);
  if (!d) return '—';
  return d.toLocaleString(I18n.getLanguage(), { dateStyle: 'short', timeStyle: 'short' });
}

export function fmtAgo(s) {
  const d = parseServerTs(s);
  if (!d) return T('labs.activity_never');
  const secs = Math.max(0, Math.round((Date.now() - d.getTime()) / 1000));
  if (secs < 60) return T('ago_seconds', { n: secs });
  if (secs < 3600) return T('ago_minutes', { n: Math.round(secs / 60) });
  if (secs < 86400) return T('ago_hours', { n: Math.round(secs / 3600) });
  return T('ago_days', { n: Math.round(secs / 86400) });
}

// Two letters for an avatar; a single-word name gives its first two letters so
// no avatar is ever a lone glyph.
export function initials(name) {
  const parts = String(name || '').trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return '?';
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
  return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
}

export function errMessage(e) {
  return e?.message ? String(e.message) : String(e ?? '');
}
