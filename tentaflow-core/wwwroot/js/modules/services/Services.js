// =============================================================================
// Plik: modules/services/Services.js
// Opis: Widok zarzadzania serwisami - tabela CRUD ze statusem QUIC.
// Przyklad: ViewRouter.register('services', Services);
// =============================================================================

const Services = (() => {
  'use strict';

  let servicesList = [];
  let quicStatusMap = {};
  let statusInterval = null;

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="card">
        <div class="card-header">
          <h3 data-i18n="services.title">${I18n.t('services.title')}</h3>
          <button class="btn btn-primary btn-sm" id="btn-add-service" data-i18n="services.add_service">+ ${I18n.t('services.add_service')}</button>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th data-i18n="common.name">${I18n.t('common.name')}</th>
                  <th data-i18n="common.type">${I18n.t('common.type')}</th>
                  <th>${I18n.t('services.node')}</th>
                  <th data-i18n="services.quic_address">${I18n.t('services.quic_address')}</th>
                  <th data-i18n="services.quic_status">${I18n.t('services.quic_status')}</th>
                  <th data-i18n="common.created_at">${I18n.t('common.created_at')}</th>
                  <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                </tr>
              </thead>
              <tbody id="services-list-tbody">
                <tr>
                  <td colspan="7">
                    <div class="empty-state">
                      <div class="empty-state-text" data-i18n="common.loading">${I18n.t('common.loading')}</div>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>
    `;
  }

  // Montowanie - zaladuj dane, podepnij zdarzenia
  function mount() {
    loadServices();
    loadQuicStatus();
    statusInterval = setInterval(loadQuicStatus, 5000);

    const addBtn = document.getElementById('btn-add-service');
    if (addBtn) {
      addBtn.addEventListener('click', () => ServiceCatalog.show(null, 'services'));
    }

    const tbody = document.getElementById('services-list-tbody');
    if (tbody) {
      tbody.addEventListener('click', handleTableClick);
    }
  }

  // Odmontowanie
  function unmount() {
    const tbody = document.getElementById('services-list-tbody');
    if (tbody) {
      tbody.removeEventListener('click', handleTableClick);
    }
    servicesList = [];
    quicStatusMap = {};
    if (statusInterval) {
      clearInterval(statusInterval);
      statusInterval = null;
    }
  }

  // Pobranie statusu polaczen QUIC
  async function loadQuicStatus() {
    try {
      const status = await ApiClient.get('/api/services/status');
      quicStatusMap = status || {};
      updateQuicDots();
    } catch (err) {
      console.error('Blad ladowania statusu QUIC:', err);
    }
  }

  // Aktualizacja kropek statusu QUIC w tabeli
  function updateQuicDots() {
    document.querySelectorAll('[data-quic-status]').forEach(el => {
      const name = el.dataset.quicStatus;
      const raw = (quicStatusMap[name] || '').toLowerCase();
      let color = 'gray';
      let label = I18n.t('services.status.none');
      
      if (raw.includes('connected') && !raw.includes('disconnected')) {
        color = 'green'; label = I18n.t('services.status.connected');
      } else if (raw.includes('connecting')) {
        color = 'yellow'; label = I18n.t('services.status.connecting');
      } else if (raw.includes('disconnected')) {
        color = 'red'; label = I18n.t('services.status.disconnected');
      } else if (raw.includes('config error')) {
        color = 'gray'; label = I18n.t('services.status.config_error');
      } else if (raw.includes('ready')) {
        color = 'green'; label = I18n.t('services.status.ready');
      }
      el.innerHTML = `<span class="badge badge-${color === 'green' ? 'success' : color === 'yellow' ? 'warning' : color === 'red' ? 'error' : 'secondary'}"><span class="status-dot status-dot-${color}"></span>${label}</span>`;
    });
  }

  // Zaladowanie serwisow z API
  async function loadServices() {
    try {
      servicesList = await ApiClient.get('/api/services');
      renderTable();
    } catch (err) {
      console.error('Blad ladowania serwisow:', err);
      servicesList = [];
      renderTable();
    }
  }

  // Renderowanie tabeli
  function renderTable() {
    const tbody = document.getElementById('services-list-tbody');
    if (!tbody) return;

    if (servicesList.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="7">
            <div class="empty-state">
              <div class="empty-state-icon">&#9881;</div>
              <div class="empty-state-text" data-i18n="services.empty">${I18n.t('services.empty')}</div>
              <div class="empty-state-hint" data-i18n="services.empty_hint">${I18n.t('services.empty_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = servicesList.map(s => {
      const quicAddr = extractQuicAddr(s.config_json);
      const created = formatDate(s.created_at);
      const nodeName = s.node_hostname || s.node_name || extractNodeName(s) || '-';
      return `
        <tr>
          <td><strong>${Utils.escapeHtml(s.name)}</strong></td>
          <td>${Utils.escapeHtml(s.service_type)}</td>
          <td>${Utils.escapeHtml(nodeName)}</td>
          <td>${Utils.escapeHtml(quicAddr)}</td>
          <td><span data-quic-status="${Utils.escapeAttr(s.name)}">-</span></td>
          <td>${created}</td>
          <td>
            <div style="display: flex; gap: 4px;">
              <button class="btn btn-ghost btn-sm" data-edit="${Utils.escapeAttr(String(s.id))}" title="${I18n.t('common.edit')}" data-i18n-title="common.edit">&#9998;</button>
              <button class="btn btn-ghost btn-sm" data-delete="${Utils.escapeAttr(String(s.id))}" title="${I18n.t('common.delete')}" data-i18n-title="common.delete">&#10005;</button>
            </div>
          </td>
        </tr>
      `;
    }).join('');

    updateQuicDots();
  }

  // Obsluga klikniec w tabeli przez delegacje
  function handleTableClick(e) {
    // Edycja serwisu
    const editBtn = e.target.closest('[data-edit]');
    if (editBtn) {
      const id = parseInt(editBtn.dataset.edit, 10);
      const service = servicesList.find(s => s.id === id);
      if (service) ServiceForm.open(service, onServiceSaved);
      return;
    }

    // Usuwanie serwisu
    const deleteBtn = e.target.closest('[data-delete]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.delete, 10);
      const service = servicesList.find(s => s.id === id);
      if (service) confirmDelete(service);
      return;
    }
  }

  // Wyciaganie nazwy noda z serwisu
  function extractNodeName(service) {
    if (service.owner_node_id) return service.owner_node_id;
    if (!service.config_json) return null;
    try {
      const cfg = JSON.parse(service.config_json);
      return cfg.node_name || cfg.node_hostname || null;
    } catch { return null; }
  }

  // Wyciaganie adresu QUIC z config_json
  function extractQuicAddr(configJson) {
    if (!configJson) return '-';
    try {
      const parsed = JSON.parse(configJson);
      if (parsed.quic_url) return parsed.quic_url.replace('quic://', '');
      if (parsed.quic_port && parsed.agent_domain) return `${parsed.agent_domain}:${parsed.quic_port}`;
      return '-';
    } catch {
      return '-';
    }
  }

  // Potwierdzenie usuwania serwisu
  async function confirmDelete(service) {
    if (!confirm(I18n.t('services.delete_confirm').replace('{name}', service.name))) return;

    try {
      await ApiClient.delete(`/api/services/${service.id}`);
      App.showToast(I18n.t('services.delete_success').replace('{name}', service.name), 'success');
      loadServices();
    } catch (err) {
      App.showToast(I18n.t('services.delete_error').replace('{error}', err.message), 'error');
    }
  }

  // Callback po zapisie serwisu
  function onServiceSaved() {
    loadServices();
  }

  // Formatowanie daty
  function formatDate(dateStr) {
    if (!dateStr) return '-';
    try {
      const d = new Date(dateStr);
      const locale = I18n.getLanguage() === 'pl' ? 'pl-PL' : 'en-US';
      return d.toLocaleDateString(locale, { day: '2-digit', month: '2-digit', year: 'numeric' });
    } catch {
      return dateStr;
    }
  }

  return { render, mount, unmount };
})();
