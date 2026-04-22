// =============================================================================
// File: modules/meeting.js — Meeting Bot user app.
// Opis: Pelny state machine: idle → joining → active (z transkryptem + AI
//       summary) + ekrany VNC, history, settings, error. Protokol binarny
//       z codec Meeting* (api-binary-shim). Per-spotkanie kontener spawnuje
//       handler backendu (MeetingManager) po MeetingSessionStartRequest.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';

let activeSession = null;
let activeScreen = 'join'; // join | joining | active | vnc | history | settings | error
let activeTab = 'transcript'; // active screen sub-tab: transcript | actions | summary
let transcripts = [];
let lastTimestampMs = 0;
let sessions = [];
let selectedHistoryId = null;
let historyDetail = null;
let historyTab = 'summary';
let settings = {
  bot_name: 'TentaFlow Bot',
  stt_alias: '',
  tts_alias: '',
  llm_alias: '',
  ai_disclaimer: 'always',
  diarization: 'pyannote-3.1',
  auto_enroll: 'company_only',
  insights_frequency: 'auto',
  retention_days: '30',
  export_format: 'txt',
};
let pollTimer = null;
let sessionListTimer = null;
let errorMessage = '';

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function speakerColor(name) {
  if (!name) return '#6a7196';
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h << 5) - h + name.charCodeAt(i);
  return `hsl(${Math.abs(h) % 360}, 55%, 58%)`;
}

function speakerInitials(name) {
  if (!name) return '?';
  if (name.startsWith('SPEAKER_')) return '?';
  const parts = name.trim().split(/\s+/);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return name.slice(0, 2).toUpperCase();
}

function formatTime(ms) {
  try {
    return new Date(Number(ms)).toLocaleTimeString('pl-PL', { hour12: false });
  } catch (_) {
    return '';
  }
}

function formatDurationSec(sec) {
  if (!Number.isFinite(sec) || sec <= 0) return '—';
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  return `${m} min ${s}s`;
}

function detectPlatform(url) {
  const u = String(url || '').toLowerCase();
  if (u.includes('teams.microsoft.com')) return 'teams';
  if (u.includes('meet.google.com')) return 'meet';
  if (u.includes('zoom.us')) return 'zoom';
  if (u.includes('discord.gg') || u.includes('discord.com')) return 'discord';
  return 'other';
}

// ---- Data loaders ---------------------------------------------------------

async function loadSessions() {
  try {
    const resp = await ApiBinary.one('meetingSessionListRequest', { onlyMine: true });
    sessions = Array.isArray(resp?.sessions) ? resp.sessions : [];
  } catch (e) {
    sessions = [];
  }
}

async function loadActiveSession() {
  try {
    const resp = await ApiBinary.one('meetingActiveSessionRequest');
    if (resp?.hasActive && resp?.session) {
      activeSession = resp.session;
      activeScreen = 'active';
      await pollTranscripts(true);
      startPolling();
    }
  } catch (_) {
    // offline
  }
}

async function loadSettings() {
  try {
    const resp = await ApiBinary.one('meetingSettingsGetRequest');
    const list = Array.isArray(resp?.settings) ? resp.settings : [];
    for (const kv of list) {
      if (kv.key in settings) settings[kv.key] = kv.value;
    }
  } catch (_) {
    // ignore
  }
}

async function pollTranscripts(initial = false) {
  if (!activeSession) return;
  try {
    const resp = await ApiBinary.one('meetingTranscriptsListRequest', {
      sessionId: activeSession.sessionId,
      sinceMs: initial ? 0 : lastTimestampMs,
    });
    const entries = Array.isArray(resp?.entries) ? resp.entries : [];
    if (initial) transcripts = entries;
    else if (entries.length) transcripts = transcripts.concat(entries);
    if (entries.length) {
      lastTimestampMs = entries[entries.length - 1].timestampMs;
    }
    // Refresh active descriptor too.
    const det = await ApiBinary.one('meetingSessionDetailRequest', {
      sessionId: activeSession.sessionId,
      includeTranscripts: false,
    });
    if (det?.session) activeSession = det.session;
    renderActiveBody();
  } catch (_) {
    // network hiccup — silently retry next tick
  }
}

function startPolling() {
  stopPolling();
  pollTimer = setInterval(() => pollTranscripts(false), 2000);
}

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}

// ---- Actions --------------------------------------------------------------

async function onJoinClick() {
  const input = byId('meeting-url-input');
  const url = (input?.value || '').trim();
  if (!url) {
    toast(I18n.t('meeting.url_required'), 'error');
    return;
  }
  activeScreen = 'joining';
  render();
  try {
    const resp = await ApiBinary.one('meetingSessionStartRequest', {
      meetingUrl: url,
      title: '',
      platform: detectPlatform(url),
      botName: settings.bot_name || 'TentaFlow Bot',
      sttAlias: settings.stt_alias,
      ttsAlias: settings.tts_alias,
      llmAlias: settings.llm_alias,
    });
    if (resp?.session) {
      activeSession = resp.session;
      activeScreen = 'active';
      transcripts = [];
      lastTimestampMs = 0;
      render();
      await pollTranscripts(true);
      startPolling();
    } else {
      throw new Error('brak session w odpowiedzi');
    }
  } catch (e) {
    errorMessage = e?.message || I18n.t('meeting.err_generic');
    activeScreen = 'error';
    render();
  }
}

async function onLeaveClick() {
  if (!activeSession) {
    activeScreen = 'join';
    render();
    return;
  }
  stopPolling();
  const sessionId = activeSession.sessionId;
  try {
    await ApiBinary.one('meetingSessionLeaveRequest', { sessionId });
    toast(I18n.t('meeting.leave_ok'), 'success');
  } catch (e) {
    toast(`${I18n.t('meeting.leave_err')}: ${e?.message || ''}`, 'error');
  }
  activeSession = null;
  transcripts = [];
  activeScreen = 'join';
  await loadSessions();
  render();
}

async function onGenerateSummary(force = false) {
  if (!activeSession) return;
  try {
    const resp = await ApiBinary.one('meetingSummaryGenerateRequest', {
      sessionId: activeSession.sessionId,
      forceRefresh: force,
    });
    toast(I18n.t('meeting.summary_generated'), 'success');
    renderActiveBody();
    return resp;
  } catch (e) {
    toast(`${I18n.t('meeting.summary_err')}: ${e?.message || ''}`, 'error');
  }
}

async function onDownloadTranscript() {
  if (!activeSession) return;
  const lines = transcripts.map(
    (t) => `[${formatTime(t.timestampMs)}] ${t.speaker || 'Unknown'}: ${t.text || ''}`
  );
  const header =
    `# ${activeSession.title || activeSession.meetingKey}\n` +
    `# URL: ${activeSession.meetingUrl}\n` +
    `# Start: ${activeSession.startedAt}\n` +
    `# Wpisy: ${lines.length}\n\n`;
  const blob = new Blob([header + lines.join('\n') + '\n'], { type: 'text/plain;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `meeting-${activeSession.sessionId}.txt`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

async function selectHistorySession(id) {
  selectedHistoryId = id;
  historyDetail = null;
  historyTab = 'summary';
  render();
  try {
    const resp = await ApiBinary.one('meetingSessionDetailRequest', {
      sessionId: id,
      includeTranscripts: true,
    });
    historyDetail = resp;
    render();
  } catch (e) {
    toast(`${I18n.t('meeting.load_err')}: ${e?.message || ''}`, 'error');
  }
}

async function onSaveSettings() {
  const updated = [];
  const rootEl = byId('meeting-settings-form');
  if (rootEl) {
    rootEl.querySelectorAll('[data-setting]').forEach((el) => {
      const key = el.dataset.setting;
      const value = el.value ?? '';
      settings[key] = value;
      updated.push([key, value]);
    });
  }
  try {
    await ApiBinary.one('meetingSettingsUpdateRequest', { settings: updated });
    toast(I18n.t('meeting.settings_saved'), 'success');
  } catch (e) {
    toast(`${I18n.t('meeting.settings_err')}: ${e?.message || ''}`, 'error');
  }
}

// ---- Renderers ------------------------------------------------------------

function renderHeader(title, subtitle, chip, actions = '') {
  return `
    <header class="app-header meeting-header">
      <div class="title-row">
        <div class="title-ico">${sprite('meeting')}</div>
        <div>
          <h1>${escapeHtml(title)} ${chip}</h1>
          <div class="sub">${escapeHtml(subtitle)}</div>
        </div>
      </div>
      <div class="actions">${actions}</div>
    </header>`;
}

function renderJoinScreen() {
  const recent = sessions
    .slice(0, 5)
    .map(
      (s) => `
      <div class="recent-item" data-history-open="${s.sessionId}">
        <div class="ri-ico">${sprite('meeting')}</div>
        <div class="ri-body">
          <div class="ri-title">${escapeHtml(s.title || s.meetingKey)}</div>
          <div class="ri-meta">${escapeHtml(s.lastActivityAt || s.startedAt)} · ${s.entryCount} ${escapeHtml(I18n.t('meeting.entries'))}</div>
        </div>
        ${sprite('chevron-right')}
      </div>`
    )
    .join('');
  const header = renderHeader(
    I18n.t('meeting.title'),
    I18n.t('meeting.subtitle'),
    `<tf-chip status="idle" dot>${escapeHtml(I18n.t('meeting.status_idle'))}</tf-chip>`,
    `<tf-button variant="ghost" size="sm" icon="clock" id="mt-nav-history">${escapeHtml(I18n.t('meeting.nav_history'))}</tf-button>
     <tf-button variant="ghost" size="sm" icon="settings" id="mt-nav-settings">${escapeHtml(I18n.t('meeting.nav_settings'))}</tf-button>`
  );
  return `
    ${header}
    <div class="meeting-empty-hero">
      <div class="meeting-join-card">
        <div class="hero-ico">${sprite('link')}</div>
        <h2>${escapeHtml(I18n.t('meeting.join_title'))}</h2>
        <p class="hero-sub">${escapeHtml(I18n.t('meeting.join_sub'))}</p>
        <div class="meeting-input-row">
          <tf-input id="meeting-url-input" placeholder="${escapeAttr(I18n.t('meeting.url_placeholder'))}" icon="link" size="lg"></tf-input>
          <tf-button variant="primary" size="lg" icon="play" id="meeting-join-btn">${escapeHtml(I18n.t('meeting.join_button'))}</tf-button>
        </div>
        <div class="meeting-join-hint">${sprite('info')} ${escapeHtml(I18n.t('meeting.join_hint'))}</div>
        <div class="platform-badges">
          <span class="platform-badge active"><span class="ico teams"></span>Microsoft Teams</span>
          <span class="platform-badge"><span class="ico meet"></span>Google Meet</span>
          <span class="platform-badge"><span class="ico zoom"></span>Zoom</span>
          <span class="platform-badge"><span class="ico discord"></span>Discord <small>beta</small></span>
        </div>
      </div>
      <div class="meeting-side-panel">
        <div class="meeting-info-card">
          <h3>${sprite('bot')} ${escapeHtml(I18n.t('meeting.config_title'))}</h3>
          <div class="kv"><span class="k">${escapeHtml(I18n.t('meeting.bot_display_name'))}</span><span class="v">${escapeHtml(settings.bot_name || 'TentaFlow Bot')}</span></div>
          <div class="kv"><span class="k">${escapeHtml(I18n.t('meeting.stt_model'))}</span><span class="v"><code>${escapeHtml(settings.stt_alias || 'whisper-large-v3')}</code></span></div>
          <div class="kv"><span class="k">${escapeHtml(I18n.t('meeting.diarization'))}</span><span class="v"><code>${escapeHtml(settings.diarization || 'pyannote-3.1')}</code></span></div>
          <div class="kv"><span class="k">${escapeHtml(I18n.t('meeting.ai_summary'))}</span><span class="v"><code>${escapeHtml(settings.llm_alias || 'qwen-3.5-0.8b')}</code></span></div>
        </div>
        <div class="meeting-info-card">
          <h3>${sprite('clock')} ${escapeHtml(I18n.t('meeting.recent_title'))}</h3>
          <div class="recent-list">
            ${recent || `<div class="meeting-empty-hint">${escapeHtml(I18n.t('meeting.no_history'))}</div>`}
          </div>
        </div>
      </div>
    </div>`;
}

function renderJoiningScreen() {
  const header = renderHeader(
    I18n.t('meeting.title'),
    I18n.t('meeting.joining_sub'),
    `<tf-chip status="warn" dot>${escapeHtml(I18n.t('meeting.status_joining'))}</tf-chip>`,
    `<tf-button variant="danger" size="sm" icon="x" id="meeting-cancel-btn">${escapeHtml(I18n.t('meeting.cancel'))}</tf-button>`
  );
  return `
    ${header}
    <div class="meeting-joining-hero">
      <div class="meeting-joining-card">
        <div class="meeting-spinner"></div>
        <h2>${escapeHtml(I18n.t('meeting.joining_title'))}</h2>
        <p class="sub">${escapeHtml(activeSession?.meetingUrl || '')}</p>
        <div class="joining-steps">
          <div class="joining-step done"><div class="step-ico">${sprite('check')}</div><div class="step-body"><div class="step-title">${escapeHtml(I18n.t('meeting.step1_title'))}</div><div class="step-desc">${escapeHtml(I18n.t('meeting.step1_desc'))}</div></div></div>
          <div class="joining-step active"><div class="step-ico">2</div><div class="step-body"><div class="step-title">${escapeHtml(I18n.t('meeting.step2_title'))}</div><div class="step-desc">${escapeHtml(I18n.t('meeting.step2_desc'))}</div></div></div>
          <div class="joining-step pending"><div class="step-ico">3</div><div class="step-body"><div class="step-title">${escapeHtml(I18n.t('meeting.step3_title'))}</div><div class="step-desc">${escapeHtml(I18n.t('meeting.step3_desc'))}</div></div></div>
          <div class="joining-step pending"><div class="step-ico">4</div><div class="step-body"><div class="step-title">${escapeHtml(I18n.t('meeting.step4_title'))}</div><div class="step-desc">${escapeHtml(I18n.t('meeting.step4_desc'))}</div></div></div>
        </div>
      </div>
    </div>`;
}

function renderTranscriptRow(t) {
  const color = speakerColor(t.speaker);
  const initials = speakerInitials(t.speaker);
  const isAi = (t.speaker || '').toLowerCase() === 'tentaflow' || (t.model || '').startsWith('ai');
  const badge = t.isEnrolled
    ? `<span class="badge-enrolled">✓ ${escapeHtml(I18n.t('meeting.badge_enrolled'))}</span>`
    : (t.speaker || '').startsWith('SPEAKER_')
    ? `<span class="badge-temp">${escapeHtml(I18n.t('meeting.badge_temp'))}</span>`
    : '';
  const conf = t.confidence
    ? `<span class="conf">${Math.round(t.confidence * 100)}%</span>`
    : '';
  return `
    <div class="t-row${isAi ? ' ai' : ''}">
      <div class="t-avatar" style="background: ${color};">${escapeHtml(initials)}</div>
      <div class="t-body">
        <div class="t-meta">
          <span class="name">${escapeHtml(t.speaker || 'Unknown')}</span>
          <span class="time">${escapeHtml(formatTime(t.timestampMs))}</span>
          ${badge}${conf}
        </div>
        <div class="t-text">${escapeHtml(t.text || '')}</div>
      </div>
    </div>`;
}

function renderActiveScreen() {
  const s = activeSession;
  const durationMs = s ? Math.max(0, Date.now() - new Date(s.startedAt.replace(' ', 'T') + 'Z').getTime()) : 0;
  const durationLabel = formatDurationSec(durationMs / 1000);
  const chip = `<tf-chip status="success" live>${escapeHtml(I18n.t('meeting.status_live'))}</tf-chip>`;
  const title = s?.title || s?.meetingKey || I18n.t('meeting.title');
  const subtitle = `${s?.entryCount || transcripts.length} ${escapeHtml(I18n.t('meeting.entries'))} · ${durationLabel} · ${escapeHtml(s?.platform || 'teams')}`;
  const actions = `
    <tf-button variant="ghost" size="sm" icon="maximize" id="meeting-vnc-btn">${escapeHtml(I18n.t('meeting.vnc_button'))}</tf-button>
    <tf-button variant="ghost" size="sm" icon="download" id="meeting-download-btn">${escapeHtml(I18n.t('meeting.download'))}</tf-button>
    <tf-button variant="danger" size="sm" icon="log-out" id="meeting-leave-btn">${escapeHtml(I18n.t('meeting.leave_button'))}</tf-button>`;
  return `
    ${renderHeader(title, subtitle, chip, actions)}
    <div id="meeting-active-body"></div>`;
}

function renderActiveBody() {
  const mount = byId('meeting-active-body');
  if (!mount) return;
  if (activeScreen !== 'active' || !activeSession) return;
  const live = transcripts.length
    ? transcripts.map(renderTranscriptRow).join('')
    : `<div class="meeting-empty-hint" style="padding: 24px;">${escapeHtml(I18n.t('meeting.waiting_transcripts'))}</div>`;
  mount.innerHTML = `
    <div class="meeting-body">
      <div class="transcript-col">
        <div class="transcript-toolbar">
          <div class="tabs">
            <div class="tab ${activeTab === 'transcript' ? 'active' : ''}" data-active-tab="transcript">${escapeHtml(I18n.t('meeting.tab_transcript'))}</div>
            <div class="tab ${activeTab === 'summary' ? 'active' : ''}" data-active-tab="summary">${escapeHtml(I18n.t('meeting.tab_summary'))}</div>
          </div>
          <span class="count-chip">${transcripts.length} ${escapeHtml(I18n.t('meeting.entries'))}</span>
        </div>
        <div class="transcript-list" id="meeting-transcript-list">${activeTab === 'transcript' ? live : renderInlineSummary()}</div>
        <div class="live-bar">
          <span class="pulse-dot"></span>
          <span>${escapeHtml(I18n.t('meeting.live_footer'))}</span>
          <span style="margin-left:auto; font-family:'JetBrains Mono',monospace; font-size:11px;">${escapeHtml(settings.stt_alias || 'whisper-large-v3')}</span>
        </div>
      </div>
      <aside class="side-col">
        ${renderParticipantsPanel()}
        ${renderConfigPanel()}
      </aside>
    </div>`;
  // Rebind tabs
  mount.querySelectorAll('[data-active-tab]').forEach((el) => {
    el.addEventListener('click', () => {
      activeTab = el.dataset.activeTab;
      renderActiveBody();
    });
  });
}

function renderInlineSummary() {
  return `
    <div style="padding: 24px;">
      <div class="meeting-empty-hint">${escapeHtml(I18n.t('meeting.summary_live_hint'))}</div>
      <tf-button variant="primary" size="sm" icon="sparkles" id="meeting-gen-summary-btn" style="margin-top: 14px;">${escapeHtml(I18n.t('meeting.generate_summary'))}</tf-button>
    </div>`;
}

function renderParticipantsPanel() {
  const speakers = {};
  for (const t of transcripts) {
    const key = t.speaker || 'Unknown';
    if (!speakers[key]) speakers[key] = { count: 0, enrolled: t.isEnrolled, last: t.timestampMs };
    speakers[key].count += 1;
    if (t.timestampMs > speakers[key].last) speakers[key].last = t.timestampMs;
  }
  const list = Object.entries(speakers)
    .sort((a, b) => b[1].last - a[1].last)
    .map(([name, info]) => {
      const color = speakerColor(name);
      const initials = speakerInitials(name);
      const sub = info.enrolled
        ? I18n.t('meeting.enrolled')
        : name.startsWith('SPEAKER_')
        ? I18n.t('meeting.temp_speaker')
        : I18n.t('meeting.guest');
      return `
        <div class="participant">
          <div class="p-avatar" style="background: ${color};">${escapeHtml(initials)}</div>
          <div class="p-body">
            <div class="p-name">${escapeHtml(name)}</div>
            <div class="p-sub">${escapeHtml(sub)}</div>
          </div>
        </div>`;
    })
    .join('');
  return `
    <div class="panel">
      <div class="panel-head">${escapeHtml(I18n.t('meeting.participants'))} <span class="count">${Object.keys(speakers).length}</span></div>
      <div class="panel-body">${list || `<div class="meeting-empty-hint">${escapeHtml(I18n.t('meeting.no_participants'))}</div>`}</div>
    </div>`;
}

function renderConfigPanel() {
  const s = activeSession;
  return `
    <div class="panel">
      <div class="panel-head">${escapeHtml(I18n.t('meeting.backend'))}</div>
      <div class="panel-body">
        <div class="cfg-row"><span class="k">STT</span><span class="v"><code>${escapeHtml(settings.stt_alias || 'whisper-large-v3')}</code></span></div>
        <div class="cfg-row"><span class="k">${escapeHtml(I18n.t('meeting.diarization'))}</span><span class="v"><code>${escapeHtml(settings.diarization)}</code></span></div>
        <div class="cfg-row"><span class="k">LLM</span><span class="v"><code>${escapeHtml(settings.llm_alias || 'qwen-3.5-0.8b')}</code></span></div>
        <div class="cfg-row"><span class="k">QUIC port</span><span class="v">${s?.quicPort || '—'}</span></div>
        <div class="cfg-row"><span class="k">VNC port</span><span class="v">${s?.vncPort || '—'}</span></div>
        <div class="cfg-row"><span class="k">Container</span><span class="v"><code>${escapeHtml(s?.containerName || '—')}</code></span></div>
      </div>
    </div>`;
}

function renderVncScreen() {
  const s = activeSession;
  if (!s) {
    activeScreen = 'join';
    return renderJoinScreen();
  }
  const wsProtocol = location.protocol === 'https:' ? 'wss' : 'ws';
  const vncUrl = `${location.protocol}//${location.hostname}:${s.novncPort}/vnc.html?autoconnect=1&resize=scale&host=${location.hostname}&port=${s.novncPort}`;
  const chip = `<tf-chip status="success" live>VNC ${s.novncPort}</tf-chip>`;
  return `
    ${renderHeader(I18n.t('meeting.vnc_title'), `${s.title || s.meetingKey} · ${wsProtocol}://${location.hostname}:${s.novncPort}`, chip,
      `<tf-button variant="ghost" size="sm" id="meeting-vnc-back">← ${escapeHtml(I18n.t('meeting.back_to_transcript'))}</tf-button>
       <tf-button variant="danger" size="sm" icon="log-out" id="meeting-leave-btn">${escapeHtml(I18n.t('meeting.leave_button'))}</tf-button>`)}
    <div class="meeting-vnc-window">
      <iframe class="meeting-vnc-iframe" src="${escapeAttr(vncUrl)}" allowfullscreen></iframe>
      <div class="meeting-vnc-hint">${escapeHtml(I18n.t('meeting.vnc_hint'))}</div>
    </div>`;
}

function renderHistoryScreen() {
  const groups = groupSessionsByDate(sessions);
  const list = groups
    .map(
      (g) => `
      <div class="history-group-label">${escapeHtml(g.label)}</div>
      ${g.items
        .map(
          (s) => `
        <div class="history-item ${selectedHistoryId === s.sessionId ? 'active' : ''}" data-history-open="${s.sessionId}">
          <div class="hi-title">${escapeHtml(s.title || s.meetingKey)}</div>
          <div class="hi-meta">
            <span>${escapeHtml(s.startedAt)}</span>
            <span class="sep"></span>
            <span>${s.entryCount} ${escapeHtml(I18n.t('meeting.entries'))}</span>
            ${s.status === 'active' || s.status === 'joining' ? `<span class="sep"></span><tf-chip status="success" live>${escapeHtml(I18n.t('meeting.status_live'))}</tf-chip>` : ''}
          </div>
        </div>`
        )
        .join('')}`
    )
    .join('');

  const detail = renderHistoryDetail();
  const header = renderHeader(
    I18n.t('meeting.history_title'),
    `${sessions.length} ${escapeHtml(I18n.t('meeting.sessions_count'))}`,
    '',
    `<tf-button variant="ghost" size="sm" id="mt-nav-join">← ${escapeHtml(I18n.t('meeting.nav_back'))}</tf-button>`
  );
  return `
    ${header}
    <div class="meeting-history-layout">
      <aside class="history-sidebar">
        <tf-input id="meeting-history-search" placeholder="${escapeAttr(I18n.t('meeting.history_search'))}" icon="search"></tf-input>
        ${list || `<div class="meeting-empty-hint" style="padding: 20px;">${escapeHtml(I18n.t('meeting.no_history'))}</div>`}
      </aside>
      <section class="history-detail">
        ${detail}
      </section>
    </div>`;
}

function renderHistoryDetail() {
  if (!selectedHistoryId) {
    return `<div class="meeting-empty-hint" style="padding: 40px;">${escapeHtml(I18n.t('meeting.history_empty_selection'))}</div>`;
  }
  if (!historyDetail) {
    return `<div class="meeting-empty-hint" style="padding: 40px;">${escapeHtml(I18n.t('common.loading'))}</div>`;
  }
  const s = historyDetail.session;
  const tabs = `
    <div class="hd-tabs">
      <div class="hd-tab ${historyTab === 'summary' ? 'active' : ''}" data-history-tab="summary">${escapeHtml(I18n.t('meeting.tab_summary'))}</div>
      <div class="hd-tab ${historyTab === 'transcript' ? 'active' : ''}" data-history-tab="transcript">${escapeHtml(I18n.t('meeting.tab_transcript'))}</div>
    </div>`;
  const body =
    historyTab === 'summary'
      ? renderHistorySummary(historyDetail)
      : (historyDetail.transcripts || []).map(renderTranscriptRow).join('') ||
        `<div class="meeting-empty-hint">${escapeHtml(I18n.t('meeting.no_transcripts'))}</div>`;
  return `
    <div class="hd-head">
      <div>
        <h2>${escapeHtml(s.title || s.meetingKey)}</h2>
        <div class="hd-meta">
          <span>${sprite('clock')} ${escapeHtml(s.startedAt)}</span>
          <span>·</span>
          <span>${s.entryCount} ${escapeHtml(I18n.t('meeting.entries'))}</span>
        </div>
      </div>
      <div style="display:flex; gap:8px;">
        <tf-button size="sm" icon="download" id="mt-download-history">${escapeHtml(I18n.t('meeting.download'))}</tf-button>
        <tf-button variant="primary" size="sm" icon="sparkles" id="mt-regen-summary">${escapeHtml(I18n.t('meeting.regen_summary'))}</tf-button>
      </div>
    </div>
    ${tabs}
    <div class="history-body">${body}</div>`;
}

function renderHistorySummary(det) {
  const tldr = det.summaryTldr || I18n.t('meeting.no_summary');
  return `
    <div class="panel" style="margin-bottom: 16px;">
      <div class="panel-head">${sprite('sparkles')} TL;DR · ${escapeHtml(det.summaryModel || 'naive')}</div>
      <div class="panel-body" style="font-size: 13px; line-height: 1.75; color: var(--text-2);">${escapeHtml(tldr)}</div>
    </div>
    ${det.summaryDecisions ? `<div class="panel" style="margin-bottom: 16px;"><div class="panel-head">${escapeHtml(I18n.t('meeting.decisions'))}</div><div class="panel-body">${escapeHtml(det.summaryDecisions)}</div></div>` : ''}
    ${det.summaryOpenQuestions ? `<div class="panel"><div class="panel-head">${escapeHtml(I18n.t('meeting.open_questions'))}</div><div class="panel-body">${escapeHtml(det.summaryOpenQuestions)}</div></div>` : ''}`;
}

function groupSessionsByDate(list) {
  const today = [];
  const yesterday = [];
  const week = [];
  const older = [];
  const now = new Date();
  for (const s of list) {
    const t = new Date((s.startedAt || '').replace(' ', 'T') + 'Z');
    const diffMs = now.getTime() - t.getTime();
    if (diffMs < 86_400_000) today.push(s);
    else if (diffMs < 172_800_000) yesterday.push(s);
    else if (diffMs < 604_800_000) week.push(s);
    else older.push(s);
  }
  return [
    { label: I18n.t('meeting.group_today'), items: today },
    { label: I18n.t('meeting.group_yesterday'), items: yesterday },
    { label: I18n.t('meeting.group_week'), items: week },
    { label: I18n.t('meeting.group_older'), items: older },
  ].filter((g) => g.items.length > 0);
}

function renderErrorScreen() {
  const header = renderHeader(
    I18n.t('meeting.title'),
    I18n.t('meeting.error_sub'),
    `<tf-chip status="danger" dot>${escapeHtml(I18n.t('meeting.status_error'))}</tf-chip>`,
    `<tf-button variant="ghost" size="sm" id="mt-nav-join">← ${escapeHtml(I18n.t('meeting.nav_back'))}</tf-button>`
  );
  return `
    ${header}
    <div class="meeting-error-hero">
      <div class="meeting-error-card">
        <div class="err-ico">${sprite('alert')}</div>
        <h2>${escapeHtml(I18n.t('meeting.error_title'))}</h2>
        <p class="err-desc">${escapeHtml(errorMessage || I18n.t('meeting.err_generic'))}</p>
        <div class="err-actions">
          <tf-button variant="primary" icon="refresh" id="mt-retry">${escapeHtml(I18n.t('meeting.retry'))}</tf-button>
          <tf-button variant="ghost" id="mt-nav-join">← ${escapeHtml(I18n.t('meeting.back_to_start'))}</tf-button>
        </div>
      </div>
    </div>`;
}

function renderSettingsScreen() {
  const header = renderHeader(
    I18n.t('meeting.settings_title'),
    I18n.t('meeting.settings_sub'),
    '',
    `<tf-button variant="ghost" size="sm" id="mt-nav-join">← ${escapeHtml(I18n.t('meeting.nav_back'))}</tf-button>
     <tf-button variant="primary" size="sm" icon="check" id="mt-save-settings">${escapeHtml(I18n.t('meeting.save'))}</tf-button>`
  );
  const textField = (key, label, hint, placeholder = '') => `
    <div class="settings-field">
      <div><div class="field-label">${escapeHtml(label)}</div><div class="field-hint">${escapeHtml(hint)}</div></div>
      <tf-input data-setting="${key}" value="${escapeAttr(settings[key] || '')}" placeholder="${escapeAttr(placeholder)}"></tf-input>
    </div>`;
  const selectField = (key, label, hint, options) => `
    <div class="settings-field">
      <div><div class="field-label">${escapeHtml(label)}</div><div class="field-hint">${escapeHtml(hint)}</div></div>
      <select data-setting="${key}">
        ${options
          .map(
            (o) => `<option value="${escapeAttr(o.value)}" ${settings[key] === o.value ? 'selected' : ''}>${escapeHtml(o.label)}</option>`
          )
          .join('')}
      </select>
    </div>`;
  return `
    ${header}
    <div class="meeting-settings-body" id="meeting-settings-form">
      <div class="settings-section">
        <h3>${escapeHtml(I18n.t('meeting.sett_identity_title'))}</h3>
        <div class="section-sub">${escapeHtml(I18n.t('meeting.sett_identity_sub'))}</div>
        ${textField('bot_name', I18n.t('meeting.sett_bot_name'), I18n.t('meeting.sett_bot_name_hint'), 'TentaFlow Bot')}
        ${selectField('ai_disclaimer', I18n.t('meeting.sett_disclaimer'), I18n.t('meeting.sett_disclaimer_hint'), [
          { value: 'always', label: I18n.t('meeting.disclaimer_always') },
          { value: 'eu_only', label: I18n.t('meeting.disclaimer_eu') },
          { value: 'never', label: I18n.t('meeting.disclaimer_never') },
        ])}
      </div>
      <div class="settings-section">
        <h3>${escapeHtml(I18n.t('meeting.sett_pipeline_title'))}</h3>
        <div class="section-sub">${escapeHtml(I18n.t('meeting.sett_pipeline_sub'))}</div>
        ${textField('stt_alias', 'STT', I18n.t('meeting.sett_stt_hint'), 'whisper-large-v3')}
        ${textField('tts_alias', 'TTS', I18n.t('meeting.sett_tts_hint'), 'sherpa-onnx')}
        ${selectField('diarization', I18n.t('meeting.sett_diarization'), I18n.t('meeting.sett_diarization_hint'), [
          { value: 'pyannote-3.1', label: 'pyannote-3.1' },
          { value: 'pyannote-2.1', label: 'pyannote-2.1' },
          { value: 'off', label: I18n.t('meeting.diarization_off') },
        ])}
        ${selectField('auto_enroll', I18n.t('meeting.sett_auto_enroll'), I18n.t('meeting.sett_auto_enroll_hint'), [
          { value: 'company_only', label: I18n.t('meeting.enroll_company') },
          { value: 'everyone', label: I18n.t('meeting.enroll_everyone') },
          { value: 'off', label: I18n.t('meeting.enroll_off') },
        ])}
      </div>
      <div class="settings-section">
        <h3>${escapeHtml(I18n.t('meeting.sett_ai_title'))}</h3>
        <div class="section-sub">${escapeHtml(I18n.t('meeting.sett_ai_sub'))}</div>
        ${textField('llm_alias', 'LLM', I18n.t('meeting.sett_llm_hint'), 'qwen-3.5-0.8b')}
        ${selectField('insights_frequency', I18n.t('meeting.sett_insights'), I18n.t('meeting.sett_insights_hint'), [
          { value: 'auto', label: I18n.t('meeting.insights_auto') },
          { value: 'on_action', label: I18n.t('meeting.insights_action') },
          { value: 'slow', label: I18n.t('meeting.insights_slow') },
          { value: 'off', label: I18n.t('meeting.insights_off') },
        ])}
      </div>
      <div class="settings-section">
        <h3>${escapeHtml(I18n.t('meeting.sett_storage_title'))}</h3>
        ${selectField('retention_days', I18n.t('meeting.sett_retention'), I18n.t('meeting.sett_retention_hint'), [
          { value: '30', label: '30 ' + I18n.t('meeting.days') },
          { value: '90', label: '90 ' + I18n.t('meeting.days') },
          { value: '365', label: '1 ' + I18n.t('meeting.year') },
          { value: 'unlimited', label: I18n.t('meeting.unlimited') },
        ])}
        ${selectField('export_format', I18n.t('meeting.sett_export'), I18n.t('meeting.sett_export_hint'), [
          { value: 'txt', label: '.txt' },
          { value: 'json', label: '.json' },
          { value: 'srt', label: '.srt' },
        ])}
      </div>
    </div>`;
}

// ---- Screen dispatcher ----------------------------------------------------

function render() {
  const host = byId('view-meeting') || byId('tab-content') || document.querySelector('.app-main');
  if (!host) return;
  const content =
    activeScreen === 'joining'
      ? renderJoiningScreen()
      : activeScreen === 'active'
      ? renderActiveScreen()
      : activeScreen === 'vnc'
      ? renderVncScreen()
      : activeScreen === 'history'
      ? renderHistoryScreen()
      : activeScreen === 'settings'
      ? renderSettingsScreen()
      : activeScreen === 'error'
      ? renderErrorScreen()
      : renderJoinScreen();
  host.innerHTML = `<div class="meeting-app-root">${content}</div>`;
  if (activeScreen === 'active') renderActiveBody();
  bindEvents();
}

function bindEvents() {
  byId('meeting-join-btn')?.addEventListener('click', onJoinClick);
  byId('meeting-leave-btn')?.addEventListener('click', onLeaveClick);
  byId('meeting-cancel-btn')?.addEventListener('click', onLeaveClick);
  byId('meeting-download-btn')?.addEventListener('click', onDownloadTranscript);
  byId('meeting-vnc-btn')?.addEventListener('click', () => {
    activeScreen = 'vnc';
    render();
  });
  byId('meeting-vnc-back')?.addEventListener('click', () => {
    activeScreen = 'active';
    render();
  });
  byId('meeting-gen-summary-btn')?.addEventListener('click', () => onGenerateSummary(true));
  byId('mt-nav-history')?.addEventListener('click', async () => {
    activeScreen = 'history';
    await loadSessions();
    render();
  });
  byId('mt-nav-settings')?.addEventListener('click', async () => {
    activeScreen = 'settings';
    await loadSettings();
    render();
  });
  byId('mt-nav-join')?.addEventListener('click', () => {
    activeScreen = activeSession ? 'active' : 'join';
    render();
  });
  byId('mt-retry')?.addEventListener('click', () => {
    activeScreen = 'join';
    errorMessage = '';
    render();
  });
  byId('mt-save-settings')?.addEventListener('click', onSaveSettings);
  byId('mt-regen-summary')?.addEventListener('click', async () => {
    if (!selectedHistoryId) return;
    await onRegenHistorySummary();
  });
  byId('mt-download-history')?.addEventListener('click', onDownloadHistory);
  // URL input enter
  const urlInput = byId('meeting-url-input');
  if (urlInput) {
    urlInput.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') onJoinClick();
    });
  }
  // History items
  document.querySelectorAll('[data-history-open]').forEach((el) => {
    el.addEventListener('click', () => {
      const id = Number(el.dataset.historyOpen);
      if (activeScreen !== 'history') activeScreen = 'history';
      selectHistorySession(id);
    });
  });
}

async function onRegenHistorySummary() {
  try {
    await ApiBinary.one('meetingSummaryGenerateRequest', {
      sessionId: selectedHistoryId,
      forceRefresh: true,
    });
    await selectHistorySession(selectedHistoryId);
    toast(I18n.t('meeting.summary_generated'), 'success');
  } catch (e) {
    toast(`${I18n.t('meeting.summary_err')}: ${e?.message || ''}`, 'error');
  }
}

function onDownloadHistory() {
  if (!historyDetail) return;
  const s = historyDetail.session;
  const lines = (historyDetail.transcripts || []).map(
    (t) => `[${formatTime(t.timestampMs)}] ${t.speaker}: ${t.text}`
  );
  const blob = new Blob([`# ${s.title || s.meetingKey}\n\n${lines.join('\n')}\n`], { type: 'text/plain;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `meeting-${s.sessionId}.txt`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

// ---- Lifecycle ------------------------------------------------------------

const MeetingScreen = {
  render() {
    return `<div id="view-meeting"></div>`;
  },
  async mount() {
    activeScreen = 'join';
    activeSession = null;
    transcripts = [];
    await Promise.all([loadSessions(), loadSettings()]);
    await loadActiveSession();
    render();
    sessionListTimer = setInterval(loadSessions, 15000);
  },
  unmount() {
    stopPolling();
    if (sessionListTimer) {
      clearInterval(sessionListTimer);
      sessionListTimer = null;
    }
    activeSession = null;
    transcripts = [];
    historyDetail = null;
  },
};

export default MeetingScreen;
