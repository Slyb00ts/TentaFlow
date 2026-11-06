// =============================================================================
// Plik: modules/system-events.js
// Opis: Dispatcher dla unsolicited SystemEvent frames (service status +
//       mesh peer status). Pokazuje toast + emituje event DOM event zeby
//       zainteresowane ekrany (Services, Mesh) mogly sie odswiezyc.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { toast } from '/js/utils.js';
import { TfToast } from '/js/components/tf-toast.js';
import { followClient } from '/js/protocol/follow-client.js';

let installed = false;

// Rate-limit — jeden toast per (kategoria, id, status) co 10s. Zapobiega spamowi
// gdy peer flickeruje (multi-path iroh) albo deploy ma burst statusow.
const lastToastAt = new Map();
const TOAST_COOLDOWN_MS = 10_000;

function shouldToast(kind, id, status) {
  const key = `${kind}:${id}:${status}`;
  const now = Date.now();
  const last = lastToastAt.get(key) || 0;
  if (now - last < TOAST_COOLDOWN_MS) return false;
  lastToastAt.set(key, now);
  // GC — usun stare entries > 60s
  for (const [k, t] of lastToastAt) {
    if (now - t > 60_000) lastToastAt.delete(k);
  }
  return true;
}

function handleSystemEvent({ body }) {
  if (!body || !body._variant) return;
  if (body._variant === 'SystemEventBody.ServiceStatusChanged') handleServiceStatus(body);
  else if (body._variant === 'SystemEventBody.MeshPeerStatusChanged') handleMeshPeerStatus(body);
  else if (body._variant === 'SystemEventBody.UserNotification') showProcessIncidentNotice(body);
}

export function init() {
  if (installed) return;
  installed = true;
  followClient((client) => client.addUnsolicitedListener(handleSystemEvent));
}

function handleServiceStatus(ev) {
  const name = ev.serviceName || '?';
  const status = String(ev.status || '').toLowerCase();
  const type = ev.serviceType || '';

  if (status === 'connected' && shouldToast('svc', name, status)) {
    toast(I18n.t('system_events.service_connected', { name, type }), 'success');
  } else if (status === 'disconnected' && shouldToast('svc', name, status)) {
    const msg = ev.message ? ` — ${ev.message}` : '';
    toast(I18n.t('system_events.service_disconnected', { name, type }) + msg, 'warn');
  }
  window.dispatchEvent(new CustomEvent('tf:service-status', { detail: ev }));
}

function handleMeshPeerStatus(ev) {
  const host = ev.hostname || (ev.nodeId || '').slice(0, 12) || '?';
  const status = String(ev.status || '').toLowerCase();
  const id = ev.nodeId || host;

  if (status === 'online' && shouldToast('peer', id, status)) {
    toast(I18n.t('system_events.peer_online', { host }), 'success');
  } else if (status === 'offline' && shouldToast('peer', id, status)) {
    toast(I18n.t('system_events.peer_offline', { host }), 'warn');
  } else if (status === 'degraded' && shouldToast('peer', id, status)) {
    toast(I18n.t('system_events.peer_degraded', { host }), 'warn');
  }
  window.dispatchEvent(new CustomEvent('tf:mesh-peer-status', { detail: ev }));
}

// `UserNotification.kind` of the server's process incident notice. Project Studio
// listens to the same variant and skips this kind.
export const PROCESS_INCIDENT_KIND = 'process_incident';

const NOTICE_MEMORY = 200;
const NOTICE_TOAST_MS = 15_000;
const seenNotices = new Set();

function incidentLink(ev) {
  try {
    const link = JSON.parse(ev.linkJson);
    if (link?.summary === true && Number.isInteger(link.count) && link.count > 0) return link;
    return typeof link?.instanceId === 'string' && typeof link.code === 'string' ? link : null;
  } catch {
    return null;
  }
}

// A mass failure arrives as a few summaries and a trickle of failures as many
// single notices; both fold into one toast whose count and title are updated in
// place (no flicker, no repeated announcement) and which counts every incident
// since it was last dismissed.
const SINGLE_TOASTS_ON_SCREEN = 3;
let summaryToast = null;
let summaryCount = 0;
let singleToasts = [];

function showSummary(count, monitor) {
  if (summaryToast?.isConnected) {
    summaryCount += count;
    summaryToast.setAttribute('title', I18n.t('bpmn.incident_notice_batch', { count: summaryCount }));
    summaryToast.refresh();
    return;
  }
  summaryCount = count;
  summaryToast = TfToast.show({
    tone: 'warning',
    title: I18n.t('bpmn.incident_notice_batch', { count }),
    duration: NOTICE_TOAST_MS,
    action: {
      label: I18n.t('bpmn.incident_notice_show_runs'),
      onClick: (shown) => {
        shown.dismiss();
        monitor.openProcessInstances(null, { attention: true });
      },
    },
  });
}

// The server sends the incident code and ids, never text: the sentence is built
// here in the reader's language, from the code alone. The server may repeat a
// notice after a restart with the same id; a repeat is shown once.
export async function showProcessIncidentNotice(ev) {
  if (ev.kind !== PROCESS_INCIDENT_KIND) return false;
  const link = incidentLink(ev);
  if (!link || seenNotices.has(ev.notificationId)) return true;
  seenNotices.add(ev.notificationId);
  if (seenNotices.size > NOTICE_MEMORY) seenNotices.delete(seenNotices.values().next().value);
  const monitor = await import('/js/modules/flows-builder/process-monitor.js');
  if (link.summary) {
    showSummary(link.count, monitor);
    return true;
  }
  const sentence = link.code === 'TIMER_ERROR'
    ? I18n.t('bpmn.incident_generic')
    : monitor.processIncidentText({ code: link.code, message: '' });
  singleToasts = singleToasts.filter((shown) => shown.isConnected);
  if (singleToasts.length >= SINGLE_TOASTS_ON_SCREEN) {
    showSummary(1, monitor);
    return true;
  }
  singleToasts.push(TfToast.show({
    tone: 'warning',
    title: I18n.t('bpmn.incident_notice_title', { name: link.processName || '' }),
    message: sentence,
    duration: NOTICE_TOAST_MS,
    action: {
      label: I18n.t('bpmn.open_instance'),
      onClick: (shown) => {
        shown.dismiss();
        monitor.openProcessInstance(link.instanceId);
      },
    },
  }));
  return true;
}
