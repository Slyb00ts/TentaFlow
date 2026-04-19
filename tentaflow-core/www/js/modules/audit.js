// =============================================================================
// Plik: modules/audit.js
// Opis: Audit log — server-push subscription do AuditEvent.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast, formatDate } from '/js/utils.js';

let unsubscribe = null;
let events = [];
const MAX_EVENTS = 200;

const AuditScreen = {
  title: 'Audit log',
  render() {
    return `
      <div class="content-header">
        <h1>Audit log</h1>
        <tf-chip status="info">Live (server push)</tf-chip>
      </div>
      <div class="card" style="padding: 0;"><div id="audit-host"></div></div>`;
  },
  async mount() {
    renderEmpty();
    const client = await ApiBinary.client();
    unsubscribe = client.addUnsolicitedListener(({ envelope, body }) => {
      if (body?.variant === 'AuditEvent') {
        events.unshift(body);
        if (events.length > MAX_EVENTS) events.length = MAX_EVENTS;
        renderTable();
      }
    });
  },
  async unmount() {
    if (unsubscribe) { unsubscribe(); unsubscribe = null; }
    events = [];
  },
};

function renderEmpty() {
  const host = byId('audit-host');
  host.innerHTML = `<div class="empty-state">
    <div class="empty-state-text">Oczekiwanie na zdarzenia…</div>
    <div class="empty-state-hint">Wykonaj akcję (login, utwórz klucz API, itd.) aby zobaczyć wpis</div>
  </div>`;
}

// Mapa severity/kind zdarzen na statusy tf-chip.
function chipStatusForKind(kind) {
  const k = String(kind || '').toLowerCase();
  if (k.includes('error') || k.includes('fail') || k.includes('deny')) return 'err';
  if (k.includes('warn')) return 'warn';
  if (k.includes('success') || k.includes('ok') || k.includes('created') || k.includes('deleted')) return 'ok';
  if (k.includes('info') || k.includes('login') || k.includes('logout')) return 'info';
  return 'accent';
}

function renderTable() {
  const host = byId('audit-host');
  if (events.length === 0) { renderEmpty(); return; }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr><th>Czas</th><th>Akcja</th><th>Zasób</th><th>Wiadomość</th></tr></thead>
      <tbody>${events.map((e) => `<tr>
        <td>${formatDate(e.tsEpoch)}</td>
        <td><tf-chip status="${chipStatusForKind(e.eventKind)}">${escapeHtml(e.eventKind)}</tf-chip></td>
        <td>${escapeHtml(e.resourceId ?? '—')}</td>
        <td>${escapeHtml(e.message)}</td>
      </tr>`).join('')}</tbody>
    </table>`;
}

export default AuditScreen;
