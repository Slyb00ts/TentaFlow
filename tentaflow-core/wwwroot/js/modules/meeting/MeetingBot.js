// =============================================================================
// Plik: modules/meeting/MeetingBot.js
// Opis: Panel Meeting Bot — dolaczanie do spotkan Teams, podglad VNC,
//       status bota, sterowanie join/leave.
// Przyklad: ViewRouter.register('meeting', MeetingBot);
// =============================================================================

const MeetingBot = (() => {
  'use strict';

  let abortController = null;
  let meetingState = 'idle';
  let meetingUrl = '';
  let pollingInterval = null;

  // Stany spotkania
  const STATES = {
    idle: { color: 'secondary', icon: 'idle' },
    joining: { color: 'warning', icon: 'joining' },
    connected: { color: 'success', icon: 'connected' },
    error: { color: 'error', icon: 'error' },
  };

  // Renderowanie glownego HTML
  function render() {
    return `
      <div class="meeting-bot-container">
        <div class="card">
          <div class="card-header">
            <h3 data-i18n="meeting.title">${I18n.t('meeting.title')}</h3>
            <span id="meeting-status-badge" class="meeting-status-badge meeting-status-${meetingState}" data-i18n="meeting.status_${meetingState}">
              ${I18n.t('meeting.status_' + meetingState)}
            </span>
          </div>
          <div class="card-body">
            <div class="form-group">
              <label for="meeting-url" data-i18n="meeting.url_label">${I18n.t('meeting.url_label')}</label>
              <div class="meeting-input-row">
                <input type="text" id="meeting-url" class="form-input"
                  placeholder="${I18n.t('meeting.url_placeholder')}"
                  data-i18n-placeholder="meeting.url_placeholder"
                  value="${escapeHtml(meetingUrl)}">
                <button class="btn btn-primary btn-sm" id="btn-join-meeting"
                  ${meetingState === 'joining' || meetingState === 'connected' ? 'disabled' : ''}
                  data-i18n="meeting.join">${I18n.t('meeting.join')}</button>
                <button class="btn btn-danger btn-sm" id="btn-leave-meeting"
                  ${meetingState !== 'connected' && meetingState !== 'joining' ? 'disabled' : ''}
                  data-i18n="meeting.leave">${I18n.t('meeting.leave')}</button>
              </div>
              <div class="form-hint" data-i18n="meeting.url_hint">${I18n.t('meeting.url_hint')}</div>
            </div>
          </div>
        </div>

        <div class="card" style="margin-top: var(--spacing-lg);">
          <div class="card-header">
            <h3 data-i18n="meeting.vnc_title">${I18n.t('meeting.vnc_title')}</h3>
            <button class="btn btn-secondary btn-sm" id="btn-open-vnc" data-i18n="meeting.vnc_open">${I18n.t('meeting.vnc_open')}</button>
          </div>
          <div class="card-body">
            <p class="form-hint" data-i18n="meeting.vnc_hint">${I18n.t('meeting.vnc_hint')}</p>
          </div>
        </div>

        <div class="card" style="margin-top: var(--spacing-lg);">
          <div class="card-header">
            <h3 data-i18n="meeting.info_title">${I18n.t('meeting.info_title')}</h3>
            <button class="btn btn-secondary btn-sm" id="btn-refresh-meeting-status" data-i18n="settings.refresh">${I18n.t('settings.refresh')}</button>
          </div>
          <div class="card-body">
            <div id="meeting-info-content">
              <div class="empty-state">
                <div class="empty-state-text" data-i18n="common.loading">${I18n.t('common.loading')}</div>
              </div>
            </div>
          </div>
        </div>
      </div>
    `;
  }

  // Montowanie — podpiecie zdarzen, ladowanie statusu
  function mount() {
    abortController = new AbortController();
    const signal = abortController.signal;

    const joinBtn = document.getElementById('btn-join-meeting');
    if (joinBtn) {
      joinBtn.addEventListener('click', handleJoin, { signal });
    }

    const leaveBtn = document.getElementById('btn-leave-meeting');
    if (leaveBtn) {
      leaveBtn.addEventListener('click', handleLeave, { signal });
    }

    const vncBtn = document.getElementById('btn-open-vnc');
    if (vncBtn) {
      vncBtn.addEventListener('click', handleOpenVnc, { signal });
    }

    const refreshBtn = document.getElementById('btn-refresh-meeting-status');
    if (refreshBtn) {
      refreshBtn.addEventListener('click', loadStatus, { signal });
    }

    loadStatus();
    // Auto-polling transkrypcji co 2s (niezaleznie od stanu — transkrypcje moga przychodzic
    // gdy bot jest w meetingu)
    startPolling();
  }

  // Odmontowanie
  function unmount() {
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    if (pollingInterval) {
      clearInterval(pollingInterval);
      pollingInterval = null;
    }
  }

  // Dolaczenie do spotkania
  async function handleJoin() {
    const input = document.getElementById('meeting-url');
    if (!input) return;

    const url = input.value.trim();
    if (!url) {
      App.showToast(I18n.t('meeting.url_required'), 'error');
      return;
    }

    meetingUrl = url;
    updateState('joining');

    try {
      await ApiClient.post('/api/addons/teams-bot/tools/join_meeting', {
        meeting_url: url,
      });
      App.showToast(I18n.t('meeting.join_success'), 'success');
      startPolling();
    } catch (err) {
      updateState('error');
      App.showToast(`${I18n.t('meeting.join_error')}: ${err.message}`, 'error');
    }
  }

  // Opuszczenie spotkania
  async function handleLeave() {
    updateState('idle');

    try {
      await ApiClient.post('/api/addons/teams-bot/tools/leave_meeting', {});
      App.showToast(I18n.t('meeting.leave_success'), 'success');
      stopPolling();
    } catch (err) {
      App.showToast(`${I18n.t('meeting.leave_error')}: ${err.message}`, 'error');
    }
  }

  // Otwarcie podgladu VNC w nowym oknie (unika problemu mixed content HTTPS/HTTP)
  function handleOpenVnc() {
    // TODO: dynamiczne wykrywanie hosta/portu z rejestru uslug
    const vncUrl = 'http://localhost:6080/vnc.html?autoconnect=true';
    window.open(vncUrl, 'tentaflow-vnc', 'width=1024,height=768,menubar=no,toolbar=no');
  }

  // Ladowanie statusu bota + transkrypcji
  async function loadStatus() {
    const infoContent = document.getElementById('meeting-info-content');
    if (!infoContent) return;

    try {
      const [data, transcripts] = await Promise.all([
        ApiClient.get('/api/addons/teams-bot/ui'),
        ApiClient.get('/api/meeting-bot/transcripts?limit=50').catch(() => []),
      ]);
      const config = data.config_values || {};
      const state = config.meeting_state || 'idle';

      updateState(state);

      let html = '<div class="meeting-info-grid">';
      html += renderInfoRow(I18n.t('meeting.state'), I18n.t('meeting.status_' + meetingState));
      html += renderInfoRow(I18n.t('meeting.bot_name'), config.bot_display_name || config.bot_name || '-');

      if (config.stt_alias) {
        html += renderInfoRow(I18n.t('meeting.stt_alias'), config.stt_alias);
      }
      if (config.tts_alias) {
        html += renderInfoRow(I18n.t('meeting.tts_alias'), config.tts_alias);
      }
      if (config.llm_alias) {
        html += renderInfoRow(I18n.t('meeting.llm_alias'), config.llm_alias);
      }
      html += '</div>';

      // Sekcja transkrypcji na zywo
      html += renderTranscripts(Array.isArray(transcripts) ? transcripts : []);

      infoContent.innerHTML = html;
    } catch (err) {
      infoContent.innerHTML = `
        <div class="empty-state">
          <div class="empty-state-text">${I18n.t('meeting.status_unavailable')}</div>
          <div class="empty-state-hint">${escapeHtml(err.message)}</div>
        </div>
      `;
    }
  }

  // Renderowanie listy transkrypcji
  function renderTranscripts(entries) {
    const title = I18n.t('meeting.transcripts') || 'Transcripts';
    if (!entries || entries.length === 0) {
      return `
        <div class="meeting-transcripts">
          <h4 class="meeting-transcripts-title">${escapeHtml(title)}</h4>
          <div class="empty-state" style="padding: var(--spacing-md);">
            <div class="empty-state-hint">${escapeHtml(I18n.t('meeting.no_transcripts') || 'No transcripts yet')}</div>
          </div>
        </div>
      `;
    }

    // Entries przychodza od najnowszej — renderujemy w tej kolejnosci
    const rows = entries.map((e) => {
      const time = new Date(e.timestamp_ms || Date.now()).toLocaleTimeString();
      const speaker = escapeHtml(e.speaker || 'Unknown');
      const text = escapeHtml(e.text || '');
      const model = escapeHtml(e.model || '');
      return `
        <div class="meeting-transcript-row">
          <div class="meeting-transcript-meta">
            <span class="meeting-transcript-time">${time}</span>
            <span class="meeting-transcript-speaker">${speaker}</span>
            ${model ? `<span class="meeting-transcript-model">${model}</span>` : ''}
          </div>
          <div class="meeting-transcript-text">${text}</div>
        </div>
      `;
    }).join('');

    return `
      <div class="meeting-transcripts">
        <h4 class="meeting-transcripts-title">${escapeHtml(title)} (${entries.length})</h4>
        <div class="meeting-transcript-list">${rows}</div>
      </div>
    `;
  }

  // Renderowanie wiersza informacyjnego
  function renderInfoRow(label, value) {
    return `
      <div class="meeting-info-row">
        <span class="meeting-info-label">${escapeHtml(label)}</span>
        <span class="meeting-info-value">${escapeHtml(value)}</span>
      </div>
    `;
  }

  // Aktualizacja stanu spotkania
  function updateState(state) {
    meetingState = state;

    const badge = document.getElementById('meeting-status-badge');
    if (badge) {
      badge.className = `meeting-status-badge meeting-status-${state}`;
      badge.textContent = I18n.t('meeting.status_' + state);
    }

    const joinBtn = document.getElementById('btn-join-meeting');
    const leaveBtn = document.getElementById('btn-leave-meeting');
    if (joinBtn) joinBtn.disabled = (state === 'joining' || state === 'connected');
    if (leaveBtn) leaveBtn.disabled = (state !== 'connected' && state !== 'joining');
  }

  // Polling statusu spotkania + transkrypcji (2s dla dobrej responsywnosci)
  function startPolling() {
    stopPolling();
    pollingInterval = setInterval(loadStatus, 2000);
  }

  function stopPolling() {
    if (pollingInterval) {
      clearInterval(pollingInterval);
      pollingInterval = null;
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
