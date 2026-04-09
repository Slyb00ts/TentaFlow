// =============================================================================
// Plik: modules/catalog/SttDeployWizard.js
// Opis: Wizard wdrazania STT — wybor silnika (native Whisper lub Docker).
// Przyklad: SttDeployWizard.open(nodeId);
// =============================================================================

const SttDeployWizard = (() => {
  'use strict';

  let currentNodeId = null;
  let selectedEngine = null;

  function open(nodeId) {
    currentNodeId = nodeId;
    selectedEngine = null;
    renderEngineStep();
  }

  function renderEngineStep() {
    removeModal();

    var overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.id = 'stt-wizard-overlay';

    overlay.innerHTML = `
      <div class="modal" style="max-width:640px">
        <div class="modal-header">
          <h3>${I18n.t('catalog.stt_deploy', 'Deploy Speech to Text')}</h3>
          <button class="modal-close" id="stt-wizard-close">&times;</button>
        </div>
        <div class="modal-body" id="stt-wizard-body">
          <div class="engine-selector">
            <div class="engine-card" data-engine="whisper">
              <div class="engine-card-icon">${CatalogIcons.get('stt')}</div>
              <div class="engine-card-name">Whisper<span class="badge badge-success" style="font-size:10px;margin-left:6px;">native</span></div>
              <div class="engine-card-desc">large-v3-turbo (1.6 GB) — on-device, ${I18n.t('catalog.stt_no_internet', 'no internet required')}</div>
            </div>
            <div class="engine-card" data-engine="faster-whisper">
              <div class="engine-card-icon">${CatalogIcons.get('stt')}</div>
              <div class="engine-card-name">Faster Whisper<span class="badge badge-info" style="font-size:10px;margin-left:6px;">docker</span></div>
              <div class="engine-card-desc">${I18n.t('catalog.stt_docker_desc', 'GPU-accelerated Whisper via Docker container')}</div>
            </div>
          </div>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    document.getElementById('stt-wizard-close').onclick = close;
    overlay.onclick = function(e) { if (e.target === overlay) close(); };

    var cards = overlay.querySelectorAll('.engine-card');
    cards.forEach(function(card) {
      card.onclick = function() {
        selectedEngine = card.dataset.engine;
        cards.forEach(function(c) { c.classList.remove('selected'); });
        card.classList.add('selected');

        if (selectedEngine === 'whisper') {
          renderNativeDeploy();
        } else {
          close();
          ServiceDeployModal.open(currentNodeId, {
            id: 'stt', name: 'Speech to Text', desc: 'Faster Whisper', gpu: true, defaultPort: 5030
          });
        }
      };
    });
  }

  function renderNativeDeploy() {
    var body = document.getElementById('stt-wizard-body');
    if (!body) return;

    body.innerHTML = `
      <div style="text-align:center;padding:24px 0">
        <h4>Whisper large-v3-turbo</h4>
        <p style="color:var(--color-text-muted);margin:8px 0 16px">1.6 GB — ${I18n.t('catalog.stt_native_desc', 'on-device speech recognition')}</p>
        <div id="stt-progress-area" style="display:none;margin:0 auto 16px;max-width:360px">
          <div style="background:var(--color-bg-secondary);border-radius:8px;height:8px;overflow:hidden">
            <div id="stt-progress-bar" style="width:0%;height:100%;background:var(--color-accent);border-radius:8px;transition:width 0.5s ease"></div>
          </div>
          <div id="stt-progress-text" style="font-size:12px;color:var(--color-text-muted);margin-top:6px"></div>
        </div>
        <button id="stt-native-btn" class="btn btn-primary" style="min-width:200px">
          ${I18n.t('catalog.stt_download_load', 'Download & Load')}
        </button>
        <div id="stt-native-status" style="margin-top:12px;font-size:13px"></div>
      </div>
    `;

    document.getElementById('stt-native-btn').onclick = deployNative;
  }

  async function deployNative() {
    var btn = document.getElementById('stt-native-btn');
    var status = document.getElementById('stt-native-status');
    var progressArea = document.getElementById('stt-progress-area');
    var progressBar = document.getElementById('stt-progress-bar');
    var progressText = document.getElementById('stt-progress-text');
    if (!btn) return;

    btn.disabled = true;
    btn.textContent = I18n.t('catalog.stt_downloading', 'Downloading...');
    if (progressArea) progressArea.style.display = 'block';

    // Animowany progress bar (szacunkowy — hf_hub nie daje progress callbacku)
    var progressInterval = null;
    var currentPercent = 0;
    var downloadPhase = true;

    progressInterval = setInterval(function() {
      if (downloadPhase) {
        // Symuluj progress pobierania — spowalniaj przy wyzszych wartosciach
        var increment = currentPercent < 30 ? 2 : currentPercent < 60 ? 1 : currentPercent < 85 ? 0.3 : 0.1;
        currentPercent = Math.min(currentPercent + increment, 90);
        if (progressBar) progressBar.style.width = currentPercent + '%';
        if (progressText) progressText.textContent = I18n.t('catalog.stt_download_progress', 'Downloading model...') + ' ' + Math.round(currentPercent) + '%';
      }
    }, 500);

    try {
      var resp = await fetch('/api/chat/stt/load', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': 'Bearer ' + ApiClient.getToken()
        },
        body: '{}'
      });

      clearInterval(progressInterval);
      downloadPhase = false;

      var data = await resp.json();

      if (resp.ok) {
        // Animuj do 100%
        if (progressBar) progressBar.style.width = '100%';
        if (progressText) progressText.textContent = I18n.t('catalog.stt_loading_model', 'Model loaded!');

        if (status) status.innerHTML = '<span style="color:var(--color-success)">\u2713 ' + Utils.escapeHtml(data.name || 'large-v3-turbo') + ' (' + Utils.escapeHtml(data.device || 'cpu') + ')</span>';
        btn.textContent = I18n.t('common.done', 'Done');
        btn.disabled = false;
        btn.onclick = close;
      } else {
        if (progressBar) progressBar.style.width = '0%';
        if (progressArea) progressArea.style.display = 'none';
        if (status) status.innerHTML = '<span style="color:var(--color-error)">\u2717 ' + Utils.escapeHtml(data.error || 'Error') + '</span>';
        btn.textContent = I18n.t('catalog.stt_download_load', 'Download & Load');
        btn.disabled = false;
        btn.onclick = deployNative;
      }
    } catch (e) {
      clearInterval(progressInterval);
      if (progressBar) progressBar.style.width = '0%';
      if (progressArea) progressArea.style.display = 'none';
      if (status) status.innerHTML = '<span style="color:var(--color-error)">\u2717 ' + Utils.escapeHtml(e.message) + '</span>';
      btn.textContent = I18n.t('catalog.stt_download_load', 'Download & Load');
      btn.disabled = false;
      btn.onclick = deployNative;
    }
  }

  function removeModal() {
    var el = document.getElementById('stt-wizard-overlay');
    if (el) el.remove();
  }

  function close() {
    removeModal();
  }

  return { open: open, close: close };
})();
