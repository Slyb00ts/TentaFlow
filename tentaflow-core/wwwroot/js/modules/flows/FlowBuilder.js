// =============================================================================
// Plik: modules/flows/FlowBuilder.js
// Opis: Glowny modul edytora flow - laczy palete, canvas i panel konfiguracji,
//       obsluguje zapis/odczyt flow, toolbar z akcjami.
// Przyklad: FlowBuilder.open(flowId, onClose);
// =============================================================================

const FlowBuilder = (() => {
  'use strict';

  let flowId = null;
  let flowData = null;
  let onCloseCallback = null;
  let isOpen = false;

  // Otwarcie edytora flow
  async function open(id, onClose) {
    flowId = id;
    onCloseCallback = onClose;
    isOpen = true;

    // Zaladuj dane flow i parsuj flow_json na nodes/edges
    try {
      flowData = await ApiClient.get(`/api/flows/${id}`);
      if (flowData && flowData.flow_json) {
        try {
          const parsed = JSON.parse(flowData.flow_json);
          flowData.nodes = parsed.nodes || [];
          flowData.edges = parsed.edges || [];
        } catch (e) {
          flowData.nodes = [];
          flowData.edges = [];
        }
      }
    } catch (err) {
      App.showToast(I18n.t('flows.load_error').replace('{error}', err.message), 'error');
      close();
      return;
    }

    await renderEditor();
  }

  // Renderowanie edytora
  async function renderEditor() {
    const content = document.getElementById('content');
    if (!content) return;

    const pageTitle = document.getElementById('page-title');
    if (pageTitle) pageTitle.textContent = I18n.t('flows.title');

    content.innerHTML = `
      <div class="flow-builder">
        <div class="flow-builder-toolbar">
          <button class="btn btn-ghost btn-sm" id="fb-back" title="${I18n.t('common.back')}">&larr; ${I18n.t('common.back')}</button>
          <div class="flow-toolbar-separator"></div>
          <input type="text" class="flow-name-input" id="fb-name" value="${Utils.escapeAttr(flowData?.name || I18n.t('flows.new_flow'))}">
          <div class="flow-toolbar-actions">
            <select id="fb-status">
              <option value="draft" ${flowData?.status === 'draft' ? 'selected' : ''}>${I18n.t('flows.status_list.draft')}</option>
              <option value="active" ${flowData?.status === 'active' ? 'selected' : ''}>${I18n.t('common.active')}</option>
              <option value="archived" ${flowData?.status === 'archived' ? 'selected' : ''}>${I18n.t('flows.status_list.archived')}</option>
            </select>
            <div class="flow-toolbar-separator"></div>
            <button class="btn btn-ghost btn-sm" id="fb-delete-selected" title="${I18n.t('common.delete')} (Del)" data-i18n="common.delete">${I18n.t('common.delete')}</button>
            <div class="flow-toolbar-separator"></div>
            <button class="btn btn-primary btn-sm" id="fb-save" data-i18n="common.save">${I18n.t('common.save')}</button>
          </div>
        </div>
        <div class="flow-builder-body">
          <div class="flow-palette" id="fb-palette"></div>
          <div class="flow-canvas-container" id="fb-canvas"></div>
          <div class="flow-config-panel" id="fb-config"></div>
        </div>
      </div>
    `;

    // Inicjalizacja komponentow
    const paletteEl = document.getElementById('fb-palette');
    const canvasEl = document.getElementById('fb-canvas');
    const configEl = document.getElementById('fb-config');

    // Paleta
    await FlowNodePalette.init(paletteEl);

    // Canvas
    FlowCanvas.init(canvasEl, handleCanvasChange, handleNodeSelected, handleEdgeSelected);

    // Zaladuj istniejace wezly i krawedzie
    const nodes = flowData?.nodes || [];
    const edges = flowData?.edges || [];
    FlowCanvas.setData(nodes, edges);

    // Panel konfiguracji
    FlowNodeConfig.init(configEl, handleConfigAction);

    // Drop na canvas
    canvasEl.addEventListener('dragover', (e) => {
      e.preventDefault();
      e.dataTransfer.dropEffect = 'copy';
      canvasEl.classList.add('drag-over');
    });

    canvasEl.addEventListener('dragleave', () => {
      canvasEl.classList.remove('drag-over');
    });

    canvasEl.addEventListener('drop', (e) => {
      e.preventDefault();
      canvasEl.classList.remove('drag-over');

      const dragData = FlowNodePalette.getDragData();
      if (dragData) {
        const node = FlowCanvas.handleDrop(dragData.type, dragData.label, e.clientX, e.clientY);
        if (node && dragData.defaultConfig) {
          node.config = { ...dragData.defaultConfig };
          FlowCanvas.updateNode(node.id, {});
        }
      }
    });

    // Toolbar
    document.getElementById('fb-back')?.addEventListener('click', close);
    document.getElementById('fb-save')?.addEventListener('click', saveFlow);
    document.getElementById('fb-delete-selected')?.addEventListener('click', () => {
      FlowCanvas.deleteSelected();
    });

    // Skrot klawiszowy Delete
    document.addEventListener('keydown', handleKeyDown);
  }

  // Obsluga zmiany na canvasie
  function handleCanvasChange() {
    // Automatycznie synchronizuj dane
  }

  // Zaznaczenie wezla na canvasie
  function handleNodeSelected(node) {
    FlowNodeConfig.showNode(node);
  }

  // Zaznaczenie krawedzi na canvasie
  function handleEdgeSelected(edge) {
    FlowNodeConfig.showEmpty();
  }

  // Akcja z panelu konfiguracji
  function handleConfigAction(nodeId, action) {
    if (action === 'delete' && nodeId) {
      FlowCanvas.removeNode(nodeId);
      FlowNodeConfig.showEmpty();
    } else if (action === 'deselect') {
      FlowCanvas.selectNode(null);
    } else if (action === 'update' && nodeId) {
      // Aktualizuj tylko zmieniony wezel zamiast pelnego przeladowania
      FlowCanvas.updateNode(nodeId, {});
    }
  }

  // Obsluga klawiszy
  function handleKeyDown(e) {
    if (!isOpen) return;
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA' || e.target.tagName === 'SELECT') return;

    if (e.key === 'Delete' || e.key === 'Backspace') {
      FlowCanvas.deleteSelected();
    }
  }

  // Zapis flow
  async function saveFlow() {
    const nameInput = document.getElementById('fb-name');
    const statusSelect = document.getElementById('fb-status');
    const canvasData = FlowCanvas.getData();

    const payload = {
      name: nameInput?.value || I18n.t('flows.new_flow'),
      status: statusSelect?.value || 'draft',
      description: flowData?.description || '',
      service_type: flowData?.service_type || '',
      flow_json: JSON.stringify({
        nodes: canvasData.nodes,
        edges: canvasData.edges,
      }),
    };

    try {
      await ApiClient.put(`/api/flows/${flowId}`, payload);
      App.showToast(I18n.t('flows.saved'), 'success');
    } catch (err) {
      App.showToast(I18n.t('flows.save_error').replace('{error}', err.message), 'error');
    }
  }

  // Zamkniecie edytora
  function close() {
    isOpen = false;
    document.removeEventListener('keydown', handleKeyDown);

    FlowCanvas.destroy();
    FlowNodePalette.destroy();
    FlowNodeConfig.destroy();

    flowId = null;
    flowData = null;

    // Powrot do listy flow
    if (onCloseCallback) {
      onCloseCallback();
    }
    ViewRouter.navigate('flows');
  }

  return { open, close };
})();
