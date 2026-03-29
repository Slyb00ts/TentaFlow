// =============================================================================
// Plik: modules/flows/FlowNodePalette.js
// Opis: Paleta dostepnych typow wezlow do Flow Buildera - laduje szablony
//       dynamicznie z API, drag&drop na canvas, wyszukiwanie, grupowanie.
// Przyklad: await FlowNodePalette.init(containerEl);
// =============================================================================

const FlowNodePalette = (() => {
  'use strict';

  let containerEl = null;
  let onDropCallback = null;
  let dragData = null;
  let templates = [];

  // Kolory wg typu wezla
  const TYPE_COLORS = {
    trigger: '#22c55e',
    llm: '#6366f1',
    stt: '#f59e0b',
    tts: '#ec4899',
    rag: '#3b82f6',
    memory: '#06b6d4',
    embeddings: '#8b5cf6',
    reranker: '#f472b6',
    condition: '#f97316',
    switch: '#a855f7',
    template: '#64748b',
    pii_filter: '#10b981',
    tts_clean: '#14b8a6',
    router: '#6b7280',
    output: '#ef4444',
  };

  // Nazwy kategorii
  const CATEGORY_LABELS = {
    trigger: 'Triggers',
    service: 'AI Services',
    transform: 'Transform',
    logic: 'Logic',
    output: 'Outputs',
  };

  // Kolejnosc kategorii
  const CATEGORY_ORDER = ['trigger', 'service', 'transform', 'logic', 'output'];

  // Inicjalizacja palety - laduje szablony z API
  async function init(container, onDrop) {
    containerEl = container;
    onDropCallback = onDrop;
    templates = await ApiClient.get('/api/flow-node-templates');
    render();
  }

  // Grupowanie szablonow wg kategorii
  function groupByCategory() {
    const groups = {};
    for (const tpl of templates) {
      const cat = tpl.category || 'other';
      if (!groups[cat]) groups[cat] = [];
      groups[cat].push(tpl);
    }
    return groups;
  }

  // Renderowanie palety
  function render() {
    if (!containerEl) return;

    const groups = groupByCategory();

    let html = `<input type="text" class="palette-search" id="palette-search" placeholder="${I18n.t('flows.search_node')}">`;

    for (const cat of CATEGORY_ORDER) {
      const items = groups[cat];
      if (!items || items.length === 0) continue;

      const catLabel = I18n.t(`flows.categories.${cat}`) || cat;
      html += `<div class="palette-category">`;
      html += `<div class="palette-category-header">${catLabel}</div>`;

      for (const tpl of items) {
        const color = TYPE_COLORS[tpl.node_type] || '#64748b';
        const localizedName = I18n.t(`flows.node_names.${tpl.node_type}`) || tpl.label || tpl.node_type;
        html += `
          <div class="palette-node" data-node-type="${tpl.node_type}" data-node-label="${Utils.escapeAttr(tpl.label)}" data-default-config="${Utils.escapeAttr(tpl.default_config || '{}')}" draggable="true" title="${Utils.escapeAttr(tpl.description || tpl.label)}">
            <span class="palette-dot" style="background: ${color};"></span>
            <span class="palette-label">${Utils.escapeHtml(localizedName)}</span>
          </div>
        `;
      }

      html += `</div>`;
    }

    containerEl.innerHTML = html;

    // Obsluga wyszukiwania z debounce
    const searchInput = containerEl.querySelector('#palette-search');
    if (searchInput) {
      let searchTimeout = null;
      searchInput.addEventListener('input', (e) => {
        clearTimeout(searchTimeout);
        searchTimeout = setTimeout(() => {
          const query = e.target.value.toLowerCase();
          containerEl.querySelectorAll('.palette-node').forEach(el => {
            const label = el.dataset.nodeLabel.toLowerCase();
            const type = el.dataset.nodeType.toLowerCase();
            const match = !query || label.includes(query) || type.includes(query);
            el.classList.toggle('hidden', !match);
          });
        }, 150);
      });
    }

    // Obsluga drag&drop
    containerEl.querySelectorAll('.palette-node').forEach(el => {
      el.addEventListener('dragstart', (e) => {
        let defaultConfig = {};
        try {
          defaultConfig = JSON.parse(el.dataset.defaultConfig || '{}');
        } catch (_) {}

        dragData = {
          type: el.dataset.nodeType,
          label: '', // Pusta etykieta = uzyj domyslnego tlumaczenia
          defaultConfig,
        };
        e.dataTransfer.setData('text/plain', JSON.stringify(dragData));
        e.dataTransfer.effectAllowed = 'copy';
      });

      el.addEventListener('dragend', () => {
        dragData = null;
      });
    });
  }

  // Pobranie danych drag
  function getDragData() {
    return dragData;
  }

  // Zniszczenie palety
  function destroy() {
    if (containerEl) containerEl.innerHTML = '';
    containerEl = null;
    dragData = null;
    templates = [];
  }

  return { init, getDragData, destroy };
})();
