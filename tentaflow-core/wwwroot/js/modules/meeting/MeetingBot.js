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
  let lastTranscripts = [];
  let lastTranscriptsSignature = '';

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
    lastTranscripts = [];
    lastTranscriptsSignature = '';
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
        ApiClient.get('/api/meeting-bot/transcripts?limit=2000').catch(() => []),
      ]);
      const config = data.config_values || {};
      const state = config.meeting_state || 'idle';

      updateState(state);

      let infoHtml = '<div class="meeting-info-grid">';
      infoHtml += renderInfoRow(I18n.t('meeting.state'), I18n.t('meeting.status_' + meetingState));
      infoHtml += renderInfoRow(I18n.t('meeting.bot_name'), config.bot_display_name || config.bot_name || '-');

      if (config.stt_alias) {
        infoHtml += renderInfoRow(I18n.t('meeting.stt_alias'), config.stt_alias);
      }
      if (config.tts_alias) {
        infoHtml += renderInfoRow(I18n.t('meeting.tts_alias'), config.tts_alias);
      }
      if (config.llm_alias) {
        infoHtml += renderInfoRow(I18n.t('meeting.llm_alias'), config.llm_alias);
      }
      infoHtml += '</div>';

      const entries = Array.isArray(transcripts) ? transcripts : [];

      // Pelna odbudowa tylko gdy kontener nie zawiera jeszcze sekcji transkrypcji
      // (pierwszy render po mount). W kolejnych cyklach aktualizujemy osobno grid
      // i liste transkrypcji — dzieki temu scroll nie resetuje sie co 2s.
      let transcriptsSection = infoContent.querySelector('.meeting-transcripts');
      if (!transcriptsSection) {
        infoContent.innerHTML = infoHtml + renderTranscriptsContainer();
        transcriptsSection = infoContent.querySelector('.meeting-transcripts');
        bindTranscriptsDownload();
      } else {
        const grid = infoContent.querySelector('.meeting-info-grid');
        if (grid) grid.outerHTML = infoHtml;
      }

      updateTranscripts(entries);
    } catch (err) {
      infoContent.innerHTML = `
        <div class="empty-state">
          <div class="empty-state-text">${I18n.t('meeting.status_unavailable')}</div>
          <div class="empty-state-hint">${escapeHtml(err.message)}</div>
        </div>
      `;
    }
  }

  // Deterministyczny kolor per speakera — hash stringa do HSL
  // Zapewnia ze ten sam mowca (imie lub SPEAKER_XX) zawsze ma ten sam kolor
  // miedzy renderami.
  function speakerColor(name) {
    let hash = 0;
    for (let i = 0; i < name.length; i++) {
      hash = (hash << 5) - hash + name.charCodeAt(i);
      hash |= 0;
    }
    const hue = Math.abs(hash) % 360;
    return `hsl(${hue}, 55%, 55%)`;
  }

  // Formatowanie confidence score 0.91 → "91%"
  function formatConfidence(score) {
    if (score == null) return '';
    return `${Math.round(score * 100)}%`;
  }

  // Szkielet sekcji transkrypcji (naglowek, przycisk pobierania, pusta lista).
  // Renderowany raz po mount — scroll zachowuje sie miedzy cyklami pollingu.
  function renderTranscriptsContainer() {
    const title = I18n.t('meeting.transcripts') || 'Transcripts';
    const download = I18n.t('meeting.download_transcripts') || 'Pobierz';
    return `
      <div class="meeting-transcripts">
        <div class="meeting-transcripts-header">
          <h4 class="meeting-transcripts-title"><span class="meeting-transcripts-label">${escapeHtml(title)}</span> <span class="meeting-transcripts-count">(0)</span></h4>
          <button type="button" class="btn btn-secondary btn-sm" id="btn-download-transcripts" disabled>${escapeHtml(download)}</button>
        </div>
        <div class="meeting-transcript-list"></div>
      </div>
    `;
  }

  // HTML pojedynczego wiersza transkrypcji
  function renderTranscriptRow(e) {
    const time = new Date(e.timestamp_ms || Date.now()).toLocaleTimeString();
    const speakerName = e.speaker || 'Unknown';
    const speaker = escapeHtml(speakerName);
    const text = escapeHtml(e.text || '');
    const model = escapeHtml(e.model || '');
    const color = speakerColor(speakerName);
    const isEnrolled = e.is_enrolled === true;
    const confidence = formatConfidence(e.confidence);

    let statusBadge = '';
    if (isEnrolled) {
      statusBadge = `<span class="meeting-transcript-badge meeting-transcript-badge-enrolled" title="${escapeHtml(I18n.t('meeting.enrolled_profile') || 'Enrolled voice profile')}">✓ ${escapeHtml(I18n.t('meeting.badge_enrolled') || 'known')}</span>`;
    } else if (speakerName.startsWith('SPEAKER_')) {
      statusBadge = `<span class="meeting-transcript-badge meeting-transcript-badge-temp" title="${escapeHtml(I18n.t('meeting.temp_speaker') || 'Temporary — not yet enrolled')}">${escapeHtml(I18n.t('meeting.badge_temp') || 'temp')}</span>`;
    }

    const confidenceHtml = confidence
      ? `<span class="meeting-transcript-confidence">${confidence}</span>`
      : '';

    return `
      <div class="meeting-transcript-row${isEnrolled ? ' meeting-transcript-row-enrolled' : ''}">
        <div class="meeting-transcript-meta">
          <span class="meeting-transcript-time">${time}</span>
          <span class="meeting-transcript-speaker" style="color: ${color};">${speaker}</span>
          ${statusBadge}
          ${confidenceHtml}
          ${model ? `<span class="meeting-transcript-model">${model}</span>` : ''}
        </div>
        <div class="meeting-transcript-text">${text}</div>
      </div>
    `;
  }

  // Sygnatura zbioru transkrypcji do wykrywania zmian — count + id pierwszego
  // (najnowszego) wpisu. Backend zwraca najnowsze wpisy z przodu.
  function transcriptsSignature(entries) {
    if (!entries.length) return '0:';
    const head = entries[0];
    const key = head.id || head.timestamp_ms || head.text || '';
    return `${entries.length}:${key}`;
  }

  // Aktualizacja listy transkrypcji BEZ resetowania scrolla.
  // Kluczowa regula: pozycja scrolla jest zapisywana przed podmiana DOM
  // i odtwarzana po niej. Gdy pojawil sie nowy wpis i uzytkownik byl juz
  // na samej gorze (najnowsze wpisy), zostawiamy scrollTop = 0 zeby zobaczyl
  // nowy komunikat. Gdy scroll byl ponizej, utrzymujemy jego wartosc.
  function updateTranscripts(entries) {
    const listEl = document.querySelector('.meeting-transcript-list');
    const countEl = document.querySelector('.meeting-transcripts-count');
    const downloadBtn = document.getElementById('btn-download-transcripts');
    if (!listEl) return;

    lastTranscripts = entries;
    if (downloadBtn) downloadBtn.disabled = entries.length === 0;
    if (countEl) countEl.textContent = `(${entries.length})`;

    if (entries.length === 0) {
      if (listEl.dataset.empty !== '1') {
        listEl.innerHTML = `
          <div class="empty-state" style="padding: var(--spacing-md);">
            <div class="empty-state-hint">${escapeHtml(I18n.t('meeting.no_transcripts') || 'No transcripts yet')}</div>
          </div>
        `;
        listEl.dataset.empty = '1';
        lastTranscriptsSignature = '0:';
      }
      return;
    }

    const signature = transcriptsSignature(entries);
    if (signature === lastTranscriptsSignature && listEl.dataset.empty !== '1') {
      return;
    }

    const wasAtTop = listEl.scrollTop <= 4;
    const prevScrollTop = listEl.scrollTop;
    const prevScrollHeight = listEl.scrollHeight;

    listEl.innerHTML = entries.map(renderTranscriptRow).join('');
    listEl.dataset.empty = '0';
    lastTranscriptsSignature = signature;

    if (wasAtTop) {
      listEl.scrollTop = 0;
    } else {
      const delta = listEl.scrollHeight - prevScrollHeight;
      listEl.scrollTop = prevScrollTop + Math.max(0, delta);
    }
  }

  // Podpiecie zdarzenia pobierania transkrypcji jako plik .txt
  function bindTranscriptsDownload() {
    const btn = document.getElementById('btn-download-transcripts');
    if (!btn) return;
    btn.addEventListener('click', handleDownloadTranscripts, {
      signal: abortController ? abortController.signal : undefined,
    });
  }

  // Eksport transkrypcji aktywnej sesji jako plik tekstowy.
  // Backend serwuje gotowy text/plain z calej historii sesji w DB
  // (bez limitu, przezywa restart procesu).
  async function handleDownloadTranscripts() {
    let activeId = null;
    try {
      const info = await ApiClient.get('/api/meeting-bot/sessions');
      activeId = info && info.active_id;
    } catch (err) {
      // brak aktywnej sesji w DB
    }

    if (activeId) {
      const token = (ApiClient.getToken && ApiClient.getToken()) || '';
      const url = `/api/meeting-bot/sessions/${activeId}/download`;
      try {
        const resp = await fetch(url, {
          headers: token ? { Authorization: `Bearer ${token}` } : {},
        });
        if (resp.ok) {
          const text = await resp.text();
          downloadTextAs(`transcript-session-${activeId}.txt`, text);
          return;
        }
      } catch (err) {
        // fallback ponizej
      }
    }

    // Fallback — eksport ring-buffera (gdy sesji nie ma w DB)
    if (!lastTranscripts.length) return;
    const ordered = [...lastTranscripts].sort((a, b) => (a.timestamp_ms || 0) - (b.timestamp_ms || 0));
    const lines = ordered.map((e) => {
      const ts = new Date(e.timestamp_ms || Date.now()).toISOString();
      return `[${ts}] ${e.speaker || 'Unknown'}: ${(e.text || '').trim()}`;
    });
    const stamp = new Date().toISOString().replace(/[:.]/g, '-');
    downloadTextAs(`transcript-${stamp}.txt`, lines.join('\n') + '\n');
  }

  function downloadTextAs(filename, text) {
    const blob = new Blob([text], { type: 'text/plain;charset=utf-8' });
    const objUrl = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = objUrl;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(objUrl);
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
