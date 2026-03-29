// =============================================================================
// Plik: modules/audit/AuditLog.js
// Opis: Widok logu audytowego — tabela z filtrami (user, addon, akcja, daty),
//       paginacja i eksport do CSV. Dostepne tylko dla adminow.
// Przyklad: ViewRouter.register('audit', AuditLog);
// =============================================================================

const AuditLog = (() => {
  'use strict';

  let logs = [];
  let abortController = null;
  let offset = 0;
  const PAGE_SIZE = 50;

  // Stan filtrow
  let filters = {
    user_id: '',
    addon_id: '',
    action: '',
    from: '',
    to: '',
  };

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="card">
        <div class="card-header">
          <h3>${I18n.t('audit.title') || 'Audit Log'}</h3>
          <div>
            <button class="btn btn-ghost btn-sm" id="btn-audit-export">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
                <polyline points="7 10 12 15 17 10"/>
                <line x1="12" y1="15" x2="12" y2="3"/>
              </svg>
              Eksport CSV
            </button>
            <button class="btn btn-ghost btn-sm" id="btn-audit-cleanup">Czyszczenie</button>
          </div>
        </div>
        <div class="card-body">
          <div class="audit-filters">
            <div class="inline-form">
              <div class="form-group form-group-inline">
                <label for="filter-user-id">Uzytkownik (ID)</label>
                <input type="number" id="filter-user-id" class="form-input form-input-sm" placeholder="ID usera" value="${filters.user_id}">
              </div>
              <div class="form-group form-group-inline">
                <label for="filter-addon-id">Addon</label>
                <input type="text" id="filter-addon-id" class="form-input form-input-sm" placeholder="ID addonu" value="${escapeHtml(filters.addon_id)}">
              </div>
              <div class="form-group form-group-inline">
                <label for="filter-action">Akcja</label>
                <input type="text" id="filter-action" class="form-input form-input-sm" placeholder="np. addon.install" value="${escapeHtml(filters.action)}">
              </div>
              <div class="form-group form-group-inline">
                <label for="filter-from">Od</label>
                <input type="datetime-local" id="filter-from" class="form-input form-input-sm" value="${filters.from}">
              </div>
              <div class="form-group form-group-inline">
                <label for="filter-to">Do</label>
                <input type="datetime-local" id="filter-to" class="form-input form-input-sm" value="${filters.to}">
              </div>
              <button class="btn btn-primary btn-sm" id="btn-audit-filter">Filtruj</button>
              <button class="btn btn-ghost btn-sm" id="btn-audit-clear">Wyczysc</button>
            </div>
          </div>
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>ID</th>
                  <th>Czas</th>
                  <th>Uzytkownik</th>
                  <th>Addon</th>
                  <th>Akcja</th>
                  <th>Zasob</th>
                  <th>Szczegoly</th>
                  <th>IP</th>
                </tr>
              </thead>
              <tbody id="audit-tbody">
                <tr><td colspan="8"><div class="empty-state"><div class="empty-state-text">${I18n.t('common.loading') || 'Ladowanie...'}</div></div></td></tr>
              </tbody>
            </table>
          </div>
          <div class="pagination" id="audit-pagination"></div>
        </div>
      </div>
    `;
  }

  // Montowanie
  function mount() {
    abortController = new AbortController();
    const signal = abortController.signal;

    loadLogs();

    const filterBtn = document.getElementById('btn-audit-filter');
    if (filterBtn) {
      filterBtn.addEventListener('click', applyFilters, { signal });
    }

    const clearBtn = document.getElementById('btn-audit-clear');
    if (clearBtn) {
      clearBtn.addEventListener('click', clearFilters, { signal });
    }

    const exportBtn = document.getElementById('btn-audit-export');
    if (exportBtn) {
      exportBtn.addEventListener('click', exportCsv, { signal });
    }

    const cleanupBtn = document.getElementById('btn-audit-cleanup');
    if (cleanupBtn) {
      cleanupBtn.addEventListener('click', openCleanupDialog, { signal });
    }

    const pagination = document.getElementById('audit-pagination');
    if (pagination) {
      pagination.addEventListener('click', handlePagination, { signal });
    }
  }

  // Odmontowanie
  function unmount() {
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    logs = [];
    offset = 0;
  }

  // Ladowanie logow
  async function loadLogs() {
    try {
      const queryParams = buildQueryParams();
      logs = await ApiClient.get(`/api/audit?${queryParams}`);
      renderTable();
      renderPagination();
    } catch (err) {
      App.showToast(`Blad ladowania logow: ${err.message}`, 'error');
    }
  }

  // Budowanie query string z filtrow
  function buildQueryParams() {
    const params = new URLSearchParams();
    params.set('offset', offset.toString());
    params.set('limit', PAGE_SIZE.toString());

    if (filters.user_id) params.set('user_id', filters.user_id);
    if (filters.addon_id) params.set('addon_id', filters.addon_id);
    if (filters.action) params.set('action', filters.action);
    if (filters.from) params.set('from', new Date(filters.from).toISOString());
    if (filters.to) params.set('to', new Date(filters.to).toISOString());

    return params.toString();
  }

  // Zastosowanie filtrow
  function applyFilters() {
    filters.user_id = document.getElementById('filter-user-id').value.trim();
    filters.addon_id = document.getElementById('filter-addon-id').value.trim();
    filters.action = document.getElementById('filter-action').value.trim();
    filters.from = document.getElementById('filter-from').value;
    filters.to = document.getElementById('filter-to').value;
    offset = 0;
    loadLogs();
  }

  // Wyczyszczenie filtrow
  function clearFilters() {
    filters = { user_id: '', addon_id: '', action: '', from: '', to: '' };
    document.getElementById('filter-user-id').value = '';
    document.getElementById('filter-addon-id').value = '';
    document.getElementById('filter-action').value = '';
    document.getElementById('filter-from').value = '';
    document.getElementById('filter-to').value = '';
    offset = 0;
    loadLogs();
  }

  // Renderowanie tabeli
  function renderTable() {
    const tbody = document.getElementById('audit-tbody');
    if (!tbody) return;

    if (!logs || logs.length === 0) {
      tbody.innerHTML = '<tr><td colspan="8"><div class="empty-state"><div class="empty-state-text">Brak wpisow audytowych</div></div></td></tr>';
      return;
    }

    tbody.innerHTML = logs.map(log => `
      <tr>
        <td>${log.id}</td>
        <td class="nowrap">${formatTimestamp(log.timestamp)}</td>
        <td>${log.user_id || '-'}</td>
        <td>${escapeHtml(log.addon_id || '-')}</td>
        <td><span class="badge badge-info">${escapeHtml(log.action)}</span></td>
        <td>${escapeHtml(log.resource || '-')}</td>
        <td class="audit-details">${escapeHtml(log.details || '-')}</td>
        <td>${escapeHtml(log.ip_address || '-')}</td>
      </tr>
    `).join('');
  }

  // Renderowanie paginacji
  function renderPagination() {
    const pag = document.getElementById('audit-pagination');
    if (!pag) return;

    const currentPage = Math.floor(offset / PAGE_SIZE) + 1;

    pag.innerHTML = `
      <button class="btn btn-xs btn-ghost" data-action="prev" ${offset === 0 ? 'disabled' : ''}>
        Poprzednia
      </button>
      <span class="pagination-info">Strona ${currentPage} (${logs.length} wpisow)</span>
      <button class="btn btn-xs btn-ghost" data-action="next" ${logs.length < PAGE_SIZE ? 'disabled' : ''}>
        Nastepna
      </button>
    `;
  }

  // Obsluga paginacji
  function handlePagination(e) {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;

    if (btn.dataset.action === 'prev' && offset > 0) {
      offset = Math.max(0, offset - PAGE_SIZE);
      loadLogs();
    }
    if (btn.dataset.action === 'next' && logs.length >= PAGE_SIZE) {
      offset += PAGE_SIZE;
      loadLogs();
    }
  }

  // Eksport CSV
  async function exportCsv() {
    try {
      const queryParams = buildQueryParams().replace(`limit=${PAGE_SIZE}`, 'limit=100000');
      const csvData = await ApiClient.get(`/api/audit/export?${queryParams}`);

      // Tworzymy plik CSV do pobrania
      const csvContent = typeof csvData === 'string' ? csvData : JSON.stringify(csvData);
      const blob = new Blob([csvContent], { type: 'text/csv;charset=utf-8;' });
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.setAttribute('href', url);
      link.setAttribute('download', `audit_log_${new Date().toISOString().split('T')[0]}.csv`);
      document.body.appendChild(link);
      link.click();
      document.body.removeChild(link);
      URL.revokeObjectURL(url);

      App.showToast('Eksport CSV pobrany', 'success');
    } catch (err) {
      App.showToast(`Blad eksportu: ${err.message}`, 'error');
    }
  }

  // Dialog czyszczenia starych logow
  function openCleanupDialog() {
    const days = prompt('Usun wpisy starsze niz ile dni? (domyslnie 90)', '90');
    if (days === null) return;

    const daysNum = parseInt(days, 10);
    if (isNaN(daysNum) || daysNum < 1) {
      App.showToast('Niepoprawna liczba dni', 'error');
      return;
    }

    if (confirm(`Czy na pewno chcesz usunac wpisy audytowe starsze niz ${daysNum} dni?`)) {
      cleanupLogs(daysNum);
    }
  }

  // Czyszczenie logow
  async function cleanupLogs(days) {
    try {
      const result = await ApiClient.delete(`/api/audit/cleanup?days=${days}`);
      const deleted = result?.deleted || 0;
      App.showToast(`Usunieto ${deleted} starych wpisow`, 'success');
      await loadLogs();
    } catch (err) {
      App.showToast(`Blad czyszczenia: ${err.message}`, 'error');
    }
  }

  // Formatowanie timestampu
  function formatTimestamp(ts) {
    if (!ts) return '-';
    try {
      const date = new Date(ts);
      return date.toLocaleString('pl-PL', {
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
      });
    } catch {
      return ts;
    }
  }

  // Escapowanie HTML
  function escapeHtml(str) {
    if (!str) return '';
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
  }

  return { render, mount, unmount };
})();
