// =============================================================================
// Plik: modules/flows/FlowList.js
// Opis: Widok listy flow - tabela CRUD z statusami, przyciskiem tworzenia
//       i nawigacja do edytora FlowBuilder.
// Przyklad: ViewRouter.register('flows', FlowList);
// =============================================================================

const FlowList = (() => {
  'use strict';

  let flowsList = [];

  // Statusy flow z etykietami
  const STATUSES = {
    draft: 'Szkic',
    active: 'Aktywny',
    archived: 'Archiwalny',
  };

  // Formatowanie daty
  function formatDate(dateStr) {
    if (!dateStr) return '-';
    try {
      return new Date(dateStr).toLocaleDateString('pl-PL', {
        day: '2-digit', month: '2-digit', year: 'numeric',
        hour: '2-digit', minute: '2-digit',
      });
    } catch {
      return dateStr;
    }
  }

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="card">
        <div class="card-header">
          <h3 data-i18n="flows.title">${I18n.t('flows.title')}</h3>
          <button class="btn btn-primary btn-sm" id="btn-add-flow" data-i18n="common.add">+ ${I18n.t('common.add')}</button>
        </div>
        <div class="card-body no-padding">
          <div class="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th data-i18n="common.name">${I18n.t('common.name')}</th>
                  <th data-i18n="common.description">${I18n.t('common.description')}</th>
                  <th data-i18n="common.status">${I18n.t('common.status')}</th>
                  <th data-i18n="flows.nodes_count">${I18n.t('flows.nodes_count')}</th>
                  <th data-i18n="flows.default">${I18n.t('flows.default')}</th>
                  <th data-i18n="flows.last_change">${I18n.t('flows.last_change')}</th>
                  <th data-i18n="common.actions">${I18n.t('common.actions')}</th>
                </tr>
              </thead>
              <tbody id="flows-list-tbody">
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
    loadFlows();

    document.getElementById('btn-add-flow')?.addEventListener('click', createNewFlow);

    // Delegacja zdarzen na tbody
    const tbody = document.getElementById('flows-list-tbody');
    if (tbody) {
      tbody.addEventListener('click', handleTableClick);
    }
  }

  // Odmontowanie
  function unmount() {
    flowsList = [];
  }

  // Zaladowanie flow z API
  async function loadFlows() {
    try {
      flowsList = await ApiClient.get('/api/flows');
      if (!Array.isArray(flowsList)) flowsList = [];
      renderTable();
    } catch (err) {
      console.error('Blad ladowania flow:', err);
      flowsList = [];
      renderTable();
    }
  }

  // Renderowanie tabeli
  function renderTable() {
    const tbody = document.getElementById('flows-list-tbody');
    if (!tbody) return;

    if (flowsList.length === 0) {
      tbody.innerHTML = `
        <tr>
          <td colspan="7">
            <div class="empty-state">
              <div class="empty-state-icon">&#9889;</div>
              <div class="empty-state-text" data-i18n="flows.empty">${I18n.t('flows.empty')}</div>
              <div class="empty-state-hint" data-i18n="flows.empty_hint">${I18n.t('flows.empty_hint')}</div>
            </div>
          </td>
        </tr>
      `;
      return;
    }

    tbody.innerHTML = flowsList.map(f => {
      const statusLabel = I18n.t(`flows.status_list.${f.status}`) || f.status || 'draft';
      const statusCls = f.status || 'draft';
      const nodeCount = f.nodes ? (Array.isArray(f.nodes) ? f.nodes.length : 0) : (f.node_count || 0);
      const defaultBadge = f.is_default
        ? `<span class="flow-default-badge" data-i18n="flows.default">${I18n.t('flows.default')}</span>`
        : '';

      return `
        <tr class="flow-row-clickable" data-flow-id="${f.id}">
          <td>
            <strong>${Utils.escapeHtml(f.name)}</strong>
            ${defaultBadge}
          </td>
          <td>${Utils.escapeHtml(f.description || '-')}</td>
          <td><span class="flow-status-badge flow-status-${statusCls}">${Utils.escapeHtml(statusLabel)}</span></td>
          <td>${nodeCount}</td>
          <td>${f.is_default ? I18n.t('common.yes') : I18n.t('common.no')}</td>
          <td>${formatDate(f.updated_at)}</td>
          <td>
            <div class="flow-list-actions">
              <button class="btn btn-ghost btn-sm" data-edit-flow="${f.id}" title="${I18n.t('common.edit')}">&#9998;</button>
              <button class="btn btn-ghost btn-sm" data-duplicate-flow="${f.id}" title="${I18n.t('flows.duplicated').replace('Flow duplicated', 'Duplicate')}">&#10697;</button>
              <button class="btn btn-ghost btn-sm" data-delete-flow="${f.id}" title="${I18n.t('common.delete')}">&#10005;</button>
            </div>
          </td>
        </tr>
      `;
    }).join('');

  }

  // Delegowany handler klikniec w tabeli
  function handleTableClick(e) {
    const editBtn = e.target.closest('[data-edit-flow]');
    if (editBtn) {
      const id = parseInt(editBtn.dataset.editFlow, 10);
      openFlowEditor(id);
      return;
    }

    const duplicateBtn = e.target.closest('[data-duplicate-flow]');
    if (duplicateBtn) {
      const id = parseInt(duplicateBtn.dataset.duplicateFlow, 10);
      duplicateFlow(id);
      return;
    }

    const deleteBtn = e.target.closest('[data-delete-flow]');
    if (deleteBtn) {
      const id = parseInt(deleteBtn.dataset.deleteFlow, 10);
      const flow = flowsList.find(f => f.id === id);
      if (flow) confirmDelete(flow);
      return;
    }

    // Klikniecie wiersza -> otworz edytor
    const row = e.target.closest('.flow-row-clickable');
    if (row && !e.target.closest('button')) {
      const id = parseInt(row.dataset.flowId, 10);
      openFlowEditor(id);
    }
  }

  // Otworz edytor flow (przelacz widok)
  function openFlowEditor(flowId) {
    FlowBuilder.open(flowId, () => {
      loadFlows();
    });
  }

  // Utwórz nowy flow
  async function createNewFlow() {
    try {
      const result = await ApiClient.post('/api/flows', {
        name: I18n.t('flows.new_flow'),
        status: 'draft',
        flow_json: JSON.stringify({ nodes: [], edges: [] }),
      });
      App.showToast(I18n.t('flows.created'), 'success');
      if (result && result.id) {
        openFlowEditor(result.id);
      } else {
        loadFlows();
      }
    } catch (err) {
      App.showToast(I18n.t('flows.create_error').replace('{error}', err.message), 'error');
    }
  }

  // Duplikacja flow - pobierz oryginал, utworz kopie
  async function duplicateFlow(flowId) {
    try {
      const original = await ApiClient.get(`/api/flows/${flowId}`);
      if (!original) return;
      const result = await ApiClient.post('/api/flows', {
        name: `${original.name} (kopia)`,
        description: original.description || '',
        service_type: original.service_type || '',
        flow_json: original.flow_json || JSON.stringify({ nodes: [], edges: [] }),
        status: 'draft',
      });
      if (result) {
        App.showToast(I18n.t('flows.duplicated'), 'success');
        await loadFlows();
      }
    } catch (err) {
      App.showToast(I18n.t('flows.duplicate_error').replace('{error}', err.message), 'error');
    }
  }

  // Potwierdzenie usuwania
  async function confirmDelete(flow) {
    if (!confirm(I18n.t('services.delete_confirm').replace('{name}', flow.name))) return;

    try {
      await ApiClient.delete(`/api/flows/${flow.id}`);
      App.showToast(I18n.t('flows.deleted').replace('{name}', flow.name), 'success');
      loadFlows();
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  }

  return { render, mount, unmount };
})();
