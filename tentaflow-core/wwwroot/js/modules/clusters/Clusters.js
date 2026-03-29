// =============================================================================
// Plik: Clusters.js
// Opis: Widok zarzadzania clusterami — CRUD, dodawanie/usuwanie nodow,
//       wybor strategii, listowy uklad z kartami.
// Przyklad: ViewRouter.register('clusters', Clusters);
// =============================================================================

const Clusters = (() => {
  'use strict';

  let clusters = [];
  let trustedNodes = [];

  // Pobranie clusterow z API
  async function loadClusters() {
    try {
      const response = await ApiClient.get('/api/clusters');
      clusters = response || [];
    } catch (e) {
      clusters = [];
    }
  }

  // Pobranie sparowanych nodow do selecta
  async function loadTrustedNodes() {
    try {
      const allNodes = await ApiClient.get('/api/mesh/nodes');
      trustedNodes = (allNodes || []).filter(n => {
        const trust = (n.trust_status || n.status || '').toLowerCase();
        return trust === 'trusted' || trust === 'paired' || trust === 'local' || n.is_local;
      });
    } catch (e) {
      trustedNodes = [];
    }
  }

  // Renderowanie widoku
  function render() {
    return `
      <div class="content-header" style="display:flex;align-items:center;justify-content:space-between;margin-bottom:var(--spacing-md);">
        <h2 data-i18n="clusters.title">${I18n.t('clusters.title')}</h2>
        <button class="btn btn-primary btn-sm" id="btn-add-cluster">
          + <span data-i18n="clusters.addCluster">${I18n.t('clusters.addCluster')}</span>
        </button>
      </div>
      <div id="clusters-list-container">
        <p data-i18n="common.loading">${I18n.t('common.loading')}</p>
      </div>
    `;
  }

  // Montowanie — zaladuj dane i podepnij zdarzenia
  async function mount() {
    const addBtn = document.getElementById('btn-add-cluster');
    if (addBtn) {
      addBtn.addEventListener('click', () => openClusterModal(null));
    }

    const container = document.getElementById('clusters-list-container');
    if (container) {
      container.addEventListener('click', handleListClick);
    }

    await Promise.all([loadClusters(), loadTrustedNodes()]);
    renderList();
  }

  // Odmontowanie
  function unmount() {
    const container = document.getElementById('clusters-list-container');
    if (container) {
      container.removeEventListener('click', handleListClick);
    }
    clusters = [];
    trustedNodes = [];
  }

  // Renderowanie listy clusterow
  function renderList() {
    const container = document.getElementById('clusters-list-container');
    if (!container) return;

    if (clusters.length === 0) {
      container.innerHTML = `
        <div class="empty-state">
          <div class="empty-state-icon">&#9881;</div>
          <div class="empty-state-text" data-i18n="clusters.noClusters">${I18n.t('clusters.noClusters')}</div>
        </div>
      `;
      return;
    }

    container.innerHTML = `
      <div class="clusters-list">
        ${clusters.map(renderClusterCard).join('')}
      </div>
    `;
  }

  // Renderowanie karty clustera
  function renderClusterCard(cluster) {
    const members = cluster.members || cluster.nodes || [];
    const strategy = cluster.strategy || 'distributed';
    const strategyLabel = I18n.t(`clusters.strategy_${strategy}`, strategy);
    const memberCount = members.length;

    // Badge strategii
    const strategyBadge = `<span class="badge badge-strategy badge-strategy-${Utils.escapeAttr(strategy)}">${Utils.escapeHtml(strategyLabel)}</span>`;

    // Tagi czlonkow
    const memberTags = members.map(m => {
      const name = m.hostname || m.node_name || m.node_id || '-';
      const role = m.role || 'worker';
      return `<span class="cluster-member-tag">${Utils.escapeHtml(name)} <span style="color:var(--color-text-muted);">(${Utils.escapeHtml(role)})</span></span>`;
    }).join('');

    const nodesCountLabel = I18n.t('clusters.nodes_count').replace('{count}', memberCount);

    return `
      <div class="cluster-card">
        <div class="cluster-card-header">
          <span class="cluster-card-title">${Utils.escapeHtml(cluster.name || cluster.id)}</span>
          <div class="cluster-card-meta">
            ${strategyBadge}
            <span class="badge badge-info">${Utils.escapeHtml(nodesCountLabel)}</span>
          </div>
        </div>
        ${cluster.description ? `<p style="font-size:var(--font-size-sm);color:var(--color-text-secondary);margin-bottom:var(--spacing-sm);">${Utils.escapeHtml(cluster.description)}</p>` : ''}
        <div class="cluster-card-members">
          ${memberTags || `<span style="color:var(--color-text-muted);">${I18n.t('clusters.no_members')}</span>`}
        </div>
        <div class="cluster-card-actions">
          <button class="btn btn-ghost btn-sm" data-edit-cluster="${Utils.escapeAttr(cluster.id)}" title="${I18n.t('common.edit')}">&#9998; ${I18n.t('common.edit')}</button>
          <button class="btn btn-ghost btn-sm" data-delete-cluster="${Utils.escapeAttr(cluster.id)}" title="${I18n.t('common.delete')}">&#10005; ${I18n.t('common.delete')}</button>
        </div>
      </div>
    `;
  }

  // Obsluga klikniec na liscie
  function handleListClick(e) {
    const editBtn = e.target.closest('[data-edit-cluster]');
    if (editBtn) {
      const id = editBtn.dataset.editCluster;
      const cluster = clusters.find(c => String(c.id) === id);
      if (cluster) openClusterModal(cluster);
      return;
    }

    const deleteBtn = e.target.closest('[data-delete-cluster]');
    if (deleteBtn) {
      const id = deleteBtn.dataset.deleteCluster;
      const cluster = clusters.find(c => String(c.id) === id);
      if (cluster) confirmDelete(cluster);
      return;
    }
  }

  // Potwierdzenie usuwania
  async function confirmDelete(cluster) {
    const msg = I18n.t('clusters.delete_confirm').replace('{name}', cluster.name || cluster.id);
    if (!confirm(msg)) return;

    try {
      await ApiClient.delete(`/api/clusters/${encodeURIComponent(cluster.id)}`);
      App.showToast(I18n.t('clusters.delete_success').replace('{name}', cluster.name), 'success');
      await loadClusters();
      renderList();
    } catch (err) {
      App.showToast(err.message || I18n.t('common.error'), 'error');
    }
  }

  // Modal tworzenia/edycji clustera
  function openClusterModal(cluster) {
    const isEdit = !!cluster;
    const title = isEdit ? I18n.t('clusters.edit_title') : I18n.t('clusters.create_title');

    // Czlonkowie przy edycji
    let currentMembers = [];
    if (cluster) {
      currentMembers = (cluster.members || cluster.nodes || []).map(m => ({
        node_id: m.node_id || m.id,
        node_name: m.hostname || m.node_name || m.node_id || '-',
        role: m.role || 'worker'
      }));
    }

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.innerHTML = `
      <div class="modal" style="max-width:600px;">
        <div class="modal-header">
          <h3>${title}</h3>
          <button class="modal-close" id="cl-modal-close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="form-group">
            <label for="cl-name">${I18n.t('clusters.name')}</label>
            <input type="text" id="cl-name" placeholder="np. GPU-Farm" value="${Utils.escapeAttr(cluster?.name || '')}">
          </div>
          <div class="form-group">
            <label for="cl-desc">${I18n.t('clusters.description')}</label>
            <input type="text" id="cl-desc" placeholder="${I18n.t('common.optional')}" value="${Utils.escapeAttr(cluster?.description || '')}">
          </div>
          <div class="form-group">
            <label for="cl-strategy">${I18n.t('clusters.strategy')}</label>
            <select id="cl-strategy">
              <option value="distributed" ${(cluster?.strategy === 'distributed' || !cluster) ? 'selected' : ''}>${I18n.t('clusters.strategy_distributed')}</option>
              <option value="replicated" ${cluster?.strategy === 'replicated' ? 'selected' : ''}>${I18n.t('clusters.strategy_replicated')}</option>
              <option value="primary_replica" ${cluster?.strategy === 'primary_replica' ? 'selected' : ''}>${I18n.t('clusters.strategy_primary_replica')}</option>
            </select>
          </div>
          ${isEdit ? renderMembersSection(currentMembers) : ''}
          <div id="cl-form-error" class="form-error" hidden></div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="cl-modal-cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="cl-modal-save">${isEdit ? I18n.t('common.save') : I18n.t('common.add')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    const closeModal = () => {
      if (overlay.parentNode) overlay.remove();
    };

    overlay.querySelector('#cl-modal-close').addEventListener('click', closeModal);
    overlay.querySelector('#cl-modal-cancel').addEventListener('click', closeModal);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) closeModal();
    });

    // Dodawanie czlonkow
    if (isEdit) {
      bindMemberActions(overlay, cluster, currentMembers, closeModal);
    }

    // Zapis
    overlay.querySelector('#cl-modal-save').addEventListener('click', async () => {
      const name = overlay.querySelector('#cl-name').value.trim();
      const description = overlay.querySelector('#cl-desc').value.trim();
      const strategy = overlay.querySelector('#cl-strategy').value;
      const errorEl = overlay.querySelector('#cl-form-error');

      if (!name) {
        if (errorEl) { errorEl.textContent = I18n.t('common.required'); errorEl.hidden = false; }
        return;
      }

      const saveBtn = overlay.querySelector('#cl-modal-save');
      if (saveBtn) { saveBtn.disabled = true; saveBtn.textContent = '...'; }

      try {
        if (isEdit) {
          await ApiClient.put(`/api/clusters/${encodeURIComponent(cluster.id)}`, { name, description, strategy });
          App.showToast(I18n.t('clusters.update_success').replace('{name}', name), 'success');
        } else {
          await ApiClient.post('/api/clusters', { name, description, strategy });
          App.showToast(I18n.t('clusters.create_success').replace('{name}', name), 'success');
        }
        closeModal();
        await loadClusters();
        renderList();
      } catch (err) {
        if (errorEl) { errorEl.textContent = err.message || I18n.t('common.error'); errorEl.hidden = false; }
      } finally {
        if (saveBtn) {
          saveBtn.disabled = false;
          saveBtn.textContent = isEdit ? I18n.t('common.save') : I18n.t('common.add');
        }
      }
    });
  }

  // Sekcja czlonkow w modalu edycji
  function renderMembersSection(members) {
    // Filtruj nody ktore nie sa juz czlonkami
    const memberIds = members.map(m => m.node_id);
    const availableNodes = trustedNodes.filter(n => !memberIds.includes(n.id));

    const nodeOptions = availableNodes.map(n =>
      `<option value="${Utils.escapeAttr(n.id)}">${Utils.escapeHtml(n.hostname || n.name || n.id)}</option>`
    ).join('');

    return `
      <div class="form-group">
        <label>${I18n.t('clusters.members')}</label>
        <div class="cluster-members-list" id="cl-members-list">
          ${members.length === 0
            ? `<span style="color:var(--color-text-muted);font-size:var(--font-size-sm);">${I18n.t('clusters.no_members')}</span>`
            : members.map(m => `
              <div class="cluster-member-row">
                <span class="member-name">${Utils.escapeHtml(m.node_name)}</span>
                <span class="member-role">${Utils.escapeHtml(m.role)}</span>
                <button class="btn btn-ghost btn-sm" data-remove-member="${Utils.escapeAttr(m.node_id)}" title="${I18n.t('common.delete')}">&#10005;</button>
              </div>
            `).join('')
          }
        </div>
        <div style="display:flex;gap:var(--spacing-xs);margin-top:var(--spacing-sm);">
          <select id="cl-add-node-select" style="flex:1;">
            <option value="">-- ${I18n.t('clusters.select_node')} --</option>
            ${nodeOptions}
          </select>
          <select id="cl-add-role-select" style="width:140px;">
            <option value="worker">${I18n.t('clusters.role_worker')}</option>
            <option value="coordinator">${I18n.t('clusters.role_coordinator')}</option>
          </select>
          <button class="btn btn-secondary btn-sm" id="cl-add-member-btn">+</button>
        </div>
      </div>
    `;
  }

  // Powiazanie zdarzen dodawania/usuwania czlonkow
  function bindMemberActions(overlay, cluster, currentMembers, closeModal) {
    const membersListEl = overlay.querySelector('#cl-members-list');
    const addBtn = overlay.querySelector('#cl-add-member-btn');

    if (membersListEl) {
      membersListEl.addEventListener('click', async (e) => {
        const removeBtn = e.target.closest('[data-remove-member]');
        if (!removeBtn) return;
        const nodeId = removeBtn.dataset.removeMember;
        try {
          await ApiClient.delete(`/api/clusters/${encodeURIComponent(cluster.id)}/members/${encodeURIComponent(nodeId)}`);
          App.showToast(I18n.t('clusters.member_removed'), 'success');
          // Odswierz dane i modal
          await loadClusters();
          const updated = clusters.find(c => String(c.id) === String(cluster.id));
          if (updated) {
            closeModal();
            openClusterModal(updated);
          }
        } catch (err) {
          App.showToast(err.message || I18n.t('common.error'), 'error');
        }
      });
    }

    if (addBtn) {
      addBtn.addEventListener('click', async () => {
        const nodeSelect = overlay.querySelector('#cl-add-node-select');
        const roleSelect = overlay.querySelector('#cl-add-role-select');
        const nodeId = nodeSelect?.value;
        const role = roleSelect?.value || 'worker';

        if (!nodeId) return;

        try {
          await ApiClient.post(`/api/clusters/${encodeURIComponent(cluster.id)}/members`, { node_id: nodeId, role });
          App.showToast(I18n.t('clusters.member_added'), 'success');
          await loadClusters();
          const updated = clusters.find(c => String(c.id) === String(cluster.id));
          if (updated) {
            closeModal();
            openClusterModal(updated);
          }
        } catch (err) {
          App.showToast(err.message || I18n.t('common.error'), 'error');
        }
      });
    }
  }

  return { render, mount, unmount };
})();
