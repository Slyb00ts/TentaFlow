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

  if (status === 'connected') {
    toast(I18n.t('system_events.service_connected', { name, type }), 'success');
  } else if (status === 'disconnected') {
    const msg = ev.message ? ` — ${ev.message}` : '';
    toast(I18n.t('system_events.service_disconnected', { name, type }) + msg, 'warn');
  }
  window.dispatchEvent(new CustomEvent('tf:service-status', { detail: ev }));
}

function handleMeshPeerStatus(ev) {
  const host = ev.hostname || (ev.nodeId || ev.node_id || '').slice(0, 12) || '?';
  const status = String(ev.status || '').toLowerCase();

  if (status === 'online') {
    toast(I18n.t('system_events.peer_online', { host }), 'success');
  } else if (status === 'offline') {
    toast(I18n.t('system_events.peer_offline', { host }), 'warn');
  } else if (status === 'degraded') {
    toast(I18n.t('system_events.peer_degraded', { host }), 'warn');
  }
  window.dispatchEvent(new CustomEvent('tf:mesh-peer-status', { detail: ev }));
}
