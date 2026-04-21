// =============================================================================
// Plik: modules/audit/AuditBinary.js
// Opis: Audit log ekran zmigrowany na binary protocol. Subskrybuje server-push
//       AuditEvent (event-push archetyp) — kazdy nowy event pojawia sie w UI
//       bez polling.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const AuditBinary = (() => {
  'use strict';
  let unsubscribe = null;
  let events = [];
  const MAX_EVENTS = 200;

  async function start() {
    const client = await ApiBinary.client();
    // P2c FIX: addUnsolicitedListener — composable, wiele screens moze sluchac
    // bez clobberowania siebie wzajemnie.
    unsubscribe = client.addUnsolicitedListener(({ envelope, body }) => {
      if (body?.variant === 'AuditEvent') {
        events.unshift(body);
        if (events.length > MAX_EVENTS) events.length = MAX_EVENTS;
        renderTable();
      }
    });
  }

  function renderTable() {
    const tbody = document.getElementById('audit-tbody');
    if (!tbody) return;
    tbody.innerHTML = events.length === 0
      ? `<tr><td colspan="4"><div class="empty-state"><div class="empty-state-text">${I18n.t('audit.empty')}</div></div></td></tr>`
      : events.map(e => `
          <tr>
            <td>${Utils.formatDate(e.tsEpoch * 1000)}</td>
            <td><span class="badge">${Utils.escapeHtml(e.eventKind)}</span></td>
            <td>${Utils.escapeHtml(e.resourceId ?? '-')}</td>
            <td>${Utils.escapeHtml(e.message)}</td>
          </tr>
        `).join('');
  }

  return {
    mount: () => {
      events = [];
      start();
    },
    unmount: () => {
      events = [];
      if (unsubscribe) {
        unsubscribe();
        unsubscribe = null;
      }
    },
  };
})();

export default AuditBinary;
