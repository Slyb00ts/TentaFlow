// =============================================================================
// Plik: modules/services/ServiceForm.js
// Opis: Modal formularz dodawania/edycji serwisu AI z walidacja.
// Przyklad: ServiceForm.open(null, onSaved); // nowy serwis
//           ServiceForm.open(service, onSaved); // edycja
// =============================================================================

const ServiceForm = (() => {
  'use strict';

  let overlay = null;
  let currentService = null;
  let onSaveCallback = null;
  let meshNodes = [];
  let clustersList = [];

  // Otwarcie modala
  async function open(service, onSaved) {
    currentService = service;
    onSaveCallback = onSaved;
    await loadDeployTargets();
    createModal();
  }

  // Pobranie nodow i clusterow do selecta "Deploy na"
  async function loadDeployTargets() {
    try {
      const [nodesData, clustersData] = await Promise.all([
        ApiClient.get('/api/mesh/nodes').catch(() => []),
        ApiClient.get('/api/clusters').catch(() => [])
      ]);
      meshNodes = (nodesData || []).filter(n => {
        return n.is_trusted === true && n.is_local !== true;
      });
      clustersList = clustersData || [];
    } catch {
      meshNodes = [];
      clustersList = [];
    }
  }

  // Tworzenie modala w DOM
  function createModal() {
    // Usun istniejacy modal (bez czyszczenia stanu)
    if (overlay && overlay.parentNode) {
      overlay.parentNode.removeChild(overlay);
      overlay = null;
    }

    const isEdit = !!currentService;
    const title = isEdit ? I18n.t('common.edit') : I18n.t('services.add_service');

    // Pomocnik selected dla typu
    function sel(value) {
      return currentService?.service_type === value ? 'selected' : '';
    }

    overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.innerHTML = `
      <div class="modal">
        <div class="modal-header">
          <h3>${title}</h3>
          <button class="modal-close" id="modal-close-btn">&times;</button>
        </div>
        <div class="modal-body">
          <form id="service-form">
            <div class="form-group">
              <label for="svc-name" data-i18n="common.name">${I18n.t('common.name')}</label>
              <input type="text" id="svc-name" placeholder="np. embeddings-bge" required
                value="${Utils.escapeAttr(currentService?.name || '')}">
            </div>

            <div class="form-group">
              <label for="svc-type" data-i18n="services.form.type">${I18n.t('services.form.type')}</label>
              <select id="svc-type" ${isEdit ? 'disabled' : ''}>
                <option value="llm" ${sel('llm')}>LLM</option>
                <option value="embedding" ${sel('embedding')}>Embedding</option>
                <option value="stt" ${sel('stt')}>STT</option>
                <option value="tts" ${sel('tts')}>TTS</option>
                <option value="rag" ${sel('rag')}>RAG</option>
                <option value="tools" ${sel('tools')}>Tools</option>
                <option value="memory" ${sel('memory')}>Memory</option>
                <option value="reranker" ${sel('reranker')}>Reranker</option>
              </select>
            </div>

            <div class="form-group">
              <label for="svc-quic-addr" data-i18n="services.form.quic_addr">${I18n.t('services.form.quic_addr')}</label>
              <input type="text" id="svc-quic-addr" placeholder="np. 192.168.11.21:5050"
                value="${Utils.escapeAttr(extractQuicAddrFromService(currentService))}">
              <div class="form-hint" data-i18n="services.form.quic_addr_hint">${I18n.t('services.form.quic_addr_hint')}</div>
            </div>

            <div class="form-group">
              <label for="svc-sni" data-i18n="services.form.sni">${I18n.t('services.form.sni')}</label>
              <input type="text" id="svc-sni" placeholder="Opcjonalne, np. gpu-server-1.local"
                value="${Utils.escapeAttr(extractSniFromService(currentService))}">
              <div class="form-hint" data-i18n="services.form.sni_hint">${I18n.t('services.form.sni_hint')}</div>
            </div>

            <div class="form-group">
              <label for="svc-deploy-target">${I18n.t('services.deploy_on')}</label>
              <select id="svc-deploy-target">
                <option value="local" ${!currentService?.node_id && !currentService?.cluster_id ? 'selected' : ''}>${I18n.t('services.deploy_local')}</option>
                ${meshNodes.map(n => {
                  const sel = currentService?.node_id === n.node_id ? 'selected' : '';
                  return `<option value="node:${Utils.escapeAttr(n.node_id)}" ${sel}>${Utils.escapeHtml(n.hostname || n.node_id)}</option>`;
                }).join('')}
                ${clustersList.map(c => {
                  const sel = currentService?.cluster_id === c.cluster_id ? 'selected' : '';
                  return `<option value="cluster:${Utils.escapeAttr(c.cluster_id)}" ${sel}>[Cluster] ${Utils.escapeHtml(c.name || c.cluster_id)}</option>`;
                }).join('')}
              </select>
            </div>

            <div id="svc-form-error" class="form-error" hidden></div>
          </form>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" id="modal-cancel-btn" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="modal-save-btn">${isEdit ? I18n.t('common.save') : I18n.t('common.add')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    // Zdarzenia
    overlay.querySelector('#modal-close-btn').addEventListener('click', close);
    overlay.querySelector('#modal-cancel-btn').addEventListener('click', close);
    overlay.querySelector('#modal-save-btn').addEventListener('click', handleSave);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) close();
    });
  }

  // Zamkniecie modala
  function close() {
    if (overlay && overlay.parentNode) {
      overlay.parentNode.removeChild(overlay);
    }
    overlay = null;
    currentService = null;
  }

  // Wyciaganie adresu QUIC z serwisu (obsluguje oba formaty: quic_url i agent_domain+quic_port)
  function extractQuicAddrFromService(service) {
    if (!service?.config_json) return '';
    try {
      const cfg = JSON.parse(service.config_json);
      if (cfg.quic_url) return cfg.quic_url.replace('quic://', '');
      if (cfg.agent_domain && cfg.quic_port) return `${cfg.agent_domain}:${cfg.quic_port}`;
      return '';
    } catch { return ''; }
  }

  // Wyciaganie domeny SNI z serwisu
  function extractSniFromService(service) {
    if (!service?.config_json) return '';
    try {
      const cfg = JSON.parse(service.config_json);
      return cfg.sni_domain || cfg.agent_domain || '';
    } catch { return ''; }
  }

  // Obsluga zapisu
  async function handleSave() {
    const name = document.getElementById('svc-name').value.trim();
    const serviceType = document.getElementById('svc-type').value;
    const quicAddr = document.getElementById('svc-quic-addr').value.trim();
    const sniDomain = document.getElementById('svc-sni').value.trim() || null;
    const deployTarget = document.getElementById('svc-deploy-target').value;

    // Parsowanie celu deploy
    let nodeId = null;
    let clusterId = null;
    if (deployTarget && deployTarget.startsWith('node:')) {
      nodeId = deployTarget.substring(5);
    } else if (deployTarget && deployTarget.startsWith('cluster:')) {
      clusterId = deployTarget.substring(8);
    }

    if (!name) {
      showFormError(I18n.t('deploy.validation.service_required'));
      return;
    }

    const configJson = JSON.stringify({
      quic_url: quicAddr ? `quic://${quicAddr}` : '',
      sni_domain: sniDomain,
    });

    const saveBtn = document.getElementById('modal-save-btn');
    if (saveBtn) {
      saveBtn.disabled = true;
      saveBtn.textContent = '...';
    }

    try {
      if (currentService) {
        await ApiClient.put(`/api/services/${currentService.id}`, {
          name,
          strategy: 'single',
          model_category: null,
          status: 'active',
          config_json: configJson,
          node_id: nodeId,
          cluster_id: clusterId,
        });
        App.showToast(I18n.t('services.form.updated').replace('{name}', name), 'success');
      } else {
        await ApiClient.post('/api/services', {
          name,
          service_type: serviceType,
          strategy: 'single',
          model_category: null,
          config_json: configJson,
          node_id: nodeId,
          cluster_id: clusterId,
        });
        App.showToast(I18n.t('services.form.created').replace('{name}', name), 'success');
      }
      close();
      if (onSaveCallback) onSaveCallback();
    } catch (err) {
      showFormError(err.message || I18n.t('common.error'));
    } finally {
      if (saveBtn) {
        saveBtn.disabled = false;
        saveBtn.textContent = currentService ? I18n.t('common.save') : I18n.t('common.add');
      }
    }
  }

  // Wyswietlenie bledu w formularzu
  function showFormError(message) {
    const el = document.getElementById('svc-form-error');
    if (el) {
      el.textContent = message;
      el.hidden = false;
    }
  }

  return { open, close };
})();
