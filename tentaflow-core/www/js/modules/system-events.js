// =============================================================================
// Plik: modules/system-events.js
// Opis: Dispatcher dla unsolicited SystemEvent frames (service status +
//       mesh peer status). Pokazuje toast + emituje event DOM event zeby
//       zainteresowane ekrany (Services, Mesh) mogly sie odswiezyc.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { toast } from '/js/utils.js';

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

export function init() {
  if (installed) return;
  installed = true;

  ApiBinary.client()
    .then((client) => {
      client.addUnsolicitedListener(({ body }) => {
        if (!body || !body.variant) return;
        if (body.variant === 'ServiceStatusChanged') handleServiceStatus(body);
        else if (body.variant === 'MeshPeerStatusChanged') handleMeshPeerStatus(body);
      });
    })
    .catch((e) => console.warn('[system-events] client not ready:', e?.message));
}

function handleServiceStatus(ev) {
  const name = ev.serviceName || ev.service_name || '?';
  const status = String(ev.status || '').toLowerCase();
  const type = ev.serviceType || ev.service_type || '';

  if (status === 'connected' && shouldToast('svc', name, status)) {
    toast(I18n.t('system_events.service_connected', { name, type }), 'success');
  } else if (status === 'disconnected' && shouldToast('svc', name, status)) {
    const msg = ev.message ? ` — ${ev.message}` : '';
    toast(I18n.t('system_events.service_disconnected', { name, type }) + msg, 'warn');
  }
  window.dispatchEvent(new CustomEvent('tf:service-status', { detail: ev }));
}

function handleMeshPeerStatus(ev) {
  const host = ev.hostname || (ev.nodeId || ev.node_id || '').slice(0, 12) || '?';
  const status = String(ev.status || '').toLowerCase();
  const id = ev.nodeId || ev.node_id || host;

  if (status === 'online' && shouldToast('peer', id, status)) {
    toast(I18n.t('system_events.peer_online', { host }), 'success');
  } else if (status === 'offline' && shouldToast('peer', id, status)) {
    toast(I18n.t('system_events.peer_offline', { host }), 'warn');
  } else if (status === 'degraded' && shouldToast('peer', id, status)) {
    toast(I18n.t('system_events.peer_degraded', { host }), 'warn');
  }
  window.dispatchEvent(new CustomEvent('tf:mesh-peer-status', { detail: ev }));
}
