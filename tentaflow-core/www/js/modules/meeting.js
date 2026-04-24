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
import { measureItemHeight, getDefaultFont, getDefaultLineHeight } from '/js/lib/text-measure.js';
import { createVirtualList } from '/js/lib/virtual-list.js';

// Wirtualizacja transkryptów — pretext text-measure + VirtualList. Pozwala
// renderować >10k wpisów bez spowalniania DOM. Heights mierzone per-row.
// T_ROW_CHROME = meta row (20) + item padding 7+7 (14) + bottom spacing (16).
const T_ROW_CHROME = 50;
const T_TEXT_MAX_WIDTH = 0.80;   // % szerokości kolumny dla tekstu wypowiedzi
const T_MIN_ROW = 64;            // avatar 36 + padding 14 + meta 14

let activeSession = null;
let activeScreen = 'join'; // join | joining | history | settings | error
let historyVlist = null;
let historyListWidth = 800;
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
let sessionListTimer = null;
let errorMessage = '';

// Lifecycle state tracked during 'joining' screen. Stage strings mirror
// LIFECYCLE_* constants in tentaflow-protocol; 'idle' means no event received
// yet (bot just spawned, first broadcast in flight).
let currentLifecycleStage = 'idle';
let currentLifecycleDetails = '';
let unsubscribeLifecycle = null;
let lifecycleTimeoutId = null;
let elapsedTimer = null;
let joinStartedAt = 0;

// Ordered pipeline the user sees. 'failed' is a terminal side-branch, not part
// of this list — handled separately in renderJoiningScreen.
const LIFECYCLE_STEPS = [
  { key: 'container_spawned', labelKey: 'meeting.lifecycle_container_spawned' },
  { key: 'browser_launched',  labelKey: 'meeting.lifecycle_browser_launched' },
  { key: 'navigating',        labelKey: 'meeting.lifecycle_navigating' },
  { key: 'prejoin_ready',     labelKey: 'meeting.lifecycle_prejoin_ready' },
  { key: 'joining',           labelKey: 'meeting.lifecycle_joining' },
  { key: 'joined',             labelKey: 'meeting.lifecycle_joined' },
];

// Safety net: bot container sometimes hangs on OAuth / lobby. Bail after 90s
// so the user sees an actionable error instead of a frozen spinner.
const LIFECYCLE_TIMEOUT_MS = 90_000;

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

async function fetchActiveSession() {
  try {
    const resp = await ApiBinary.one('meetingActiveSessionRequest');
    if (resp?.hasActive && resp?.session) return resp.session;
  } catch (_) {
    // offline — fallback below
  }
  // Fallback: sesja z listy filtrowanej per-user ze statusem live. Robimy to
  // gdy backend nie zwrocil hasActive ale sesja z kontenerem dalej zyje.
  try {
    const resp = await ApiBinary.one('meetingSessionListRequest', { onlyMine: true });
    const all = Array.isArray(resp?.sessions) ? resp.sessions : [];
    const live = all.find((s) => ['joining', 'active', 'leaving'].includes(String(s.status || '')));
    if (live) return live;
  } catch (_) { /* ignore */ }
  return null;
}

async function navigateToLive(meetingKey) {
  if (!meetingKey) return;
  const [{ openMeetingLive }, { Router }] = await Promise.all([
    import('/js/modules/meeting-live.js'),
    import('/js/router.js'),
  ]);
  openMeetingLive(meetingKey);
  Router.navigate('meeting-live');
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

// ---- Actions --------------------------------------------------------------

async function onJoinClick() {
  const input = byId('meeting-url-input');
  const url = (input?.value || '').trim();
  if (!url) {
    toast(I18n.t('meeting.url_required'), 'error');
    return;
  }
  // Zapobiega duplikatom: jesli user ma juz zywa sesje, kierujemy do podgladu
  // zamiast spawnowac drugi kontener.
  const existing = await fetchActiveSession();
  if (existing?.meetingKey) {
    activeSession = existing;
    await navigateToLive(existing.meetingKey);
    return;
  }
  activeScreen = 'joining';
  currentLifecycleStage = 'idle';
  currentLifecycleDetails = '';
  joinStartedAt = Date.now();
  startElapsedTimer();
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
    if (!resp?.session) {
      throw new Error(I18n.t('meeting.err_no_session'));
    }
    activeSession = resp.session;
    // Descriptor may already carry a stage (bot broadcasted before response
    // landed, or user reloaded mid-join). If bot is already 'joined', skip the
    // lifecycle screen; otherwise subscribe and wait for events.
    if (activeSession.lifecycleStage) currentLifecycleStage = activeSession.lifecycleStage;
    if (activeSession.lifecycleDetails) currentLifecycleDetails = activeSession.lifecycleDetails;
    if (currentLifecycleStage === 'joined') {
      cleanupJoiningWatchers();
      await navigateToLive(activeSession.meetingKey);
      return;
    }
    if (currentLifecycleStage === 'failed') {
      failJoining(currentLifecycleDetails || I18n.t('meeting.err_generic'));
      return;
    }
    await subscribeToLifecycle();
    armLifecycleTimeout();
    render();
  } catch (e) {
    failJoining(e?.message || I18n.t('meeting.err_generic'));
  }
}

function failJoining(message) {
  cleanupJoiningWatchers();
  errorMessage = message;
  activeScreen = 'error';
  render();
}

function cleanupJoiningWatchers() {
  if (unsubscribeLifecycle) {
    try { unsubscribeLifecycle(); } catch (_) { /* no-op */ }
    unsubscribeLifecycle = null;
  }
  if (lifecycleTimeoutId) {
    clearTimeout(lifecycleTimeoutId);
    lifecycleTimeoutId = null;
  }
  stopElapsedTimer();
}

function armLifecycleTimeout() {
  if (lifecycleTimeoutId) clearTimeout(lifecycleTimeoutId);
  lifecycleTimeoutId = setTimeout(() => {
    failJoining(I18n.t('meeting.err_lifecycle_timeout'));
  }, LIFECYCLE_TIMEOUT_MS);
}

// Refreshes only the elapsed counter in place so we do not re-render the whole
// joining screen every second (and thus do not disturb keyboard focus).
function startElapsedTimer() {
  stopElapsedTimer();
  elapsedTimer = setInterval(() => {
    const el = byId('meeting-joining-elapsed');
    if (el) el.textContent = formatElapsed(Date.now() - joinStartedAt);
  }, 1000);
}

function stopElapsedTimer() {
  if (elapsedTimer) {
    clearInterval(elapsedTimer);
    elapsedTimer = null;
  }
}

function formatElapsed(ms) {
  const sec = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
}

async function subscribeToLifecycle() {
  try {
    const client = await ApiBinary.client();
    unsubscribeLifecycle = client.addUnsolicitedListener(({ body }) => {
      if (!body || body.variant !== 'MeetingLiveEventBody') return;
      if (!activeSession || body.meetingKey !== activeSession.meetingKey) return;
      const payload = body.payload;
      if (!payload || payload.type !== 'LifecycleUpdate') return;
      const data = payload.data || {};
      currentLifecycleStage = String(data.stage || currentLifecycleStage);
      currentLifecycleDetails = data.details ? String(data.details) : '';
      if (currentLifecycleStage === 'joined') {
        const key = activeSession.meetingKey;
        cleanupJoiningWatchers();
        navigateToLive(key);
        return;
      }
      if (currentLifecycleStage === 'failed') {
        failJoining(currentLifecycleDetails || I18n.t('meeting.err_generic'));
        return;
      }
      // Each forward-progress event extends the timeout; the bot is alive.
      armLifecycleTimeout();
      if (activeScreen === 'joining') render();
    });
  } catch (e) {
    console.warn('[meeting] subscribeToLifecycle failed:', e?.message);
  }
}

async function onCancelJoining() {
  cleanupJoiningWatchers();
  if (!activeSession) {
    activeScreen = 'join';
    render();
    return;
  }
  const sessionId = activeSession.sessionId;
  try {
    await ApiBinary.one('meetingSessionLeaveRequest', { sessionId });
    toast(I18n.t('meeting.leave_ok'), 'success');
  } catch (e) {
    toast(`${I18n.t('meeting.leave_err')}: ${e?.message || ''}`, 'error');
  }
  activeSession = null;
  activeScreen = 'join';
  await loadSessions();
  render();
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
  return `
    <tf-screen>
      <div slot="breadcrumb" class="tf-breadcrumb">
        <span class="crumb current">${escapeHtml(I18n.t('meeting.title'))}</span>
      </div>
      <div slot="header" class="tf-detail-header">
        <div class="big-ico"><svg viewBox="0 0 24 24"><use href="#i-meeting"/></svg></div>
        <div class="d-meta">
          <div class="d-name">
            ${escapeHtml(I18n.t('meeting.title'))}
            <tf-chip status="idle" dot>${escapeHtml(I18n.t('meeting.status_idle'))}</tf-chip>
          </div>
          <div class="d-sub">${escapeHtml(I18n.t('meeting.subtitle'))}</div>
        </div>
        <div class="d-actions">
          <tf-button variant="ghost" icon="clock" id="mt-nav-history">${escapeHtml(I18n.t('meeting.nav_history'))}</tf-button>
          <tf-button variant="ghost" icon="settings" id="mt-nav-settings">${escapeHtml(I18n.t('meeting.nav_settings'))}</tf-button>
        </div>
      </div>
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
          <div class="tf-section-card">
            <h3>${escapeHtml(I18n.t('meeting.config_title'))}</h3>
            <div class="meet-kv">
              <div class="meet-kv-row"><span class="k">${escapeHtml(I18n.t('meeting.bot_display_name'))}</span><span class="v plain">${escapeHtml(settings.bot_name || 'TentaFlow Bot')}</span></div>
              <div class="meet-kv-row"><span class="k">${escapeHtml(I18n.t('meeting.stt_model'))}</span><span class="v">${escapeHtml(settings.stt_alias || 'whisper-large-v3')}</span></div>
              <div class="meet-kv-row"><span class="k">${escapeHtml(I18n.t('meeting.diarization'))}</span><span class="v">${escapeHtml(settings.diarization || 'pyannote-3.1')}</span></div>
              <div class="meet-kv-row"><span class="k">${escapeHtml(I18n.t('meeting.ai_summary'))}</span><span class="v">${escapeHtml(settings.llm_alias || 'qwen-3.5-0.8b')}</span></div>
            </div>
          </div>
          <div class="tf-section-card">
            <h3>${escapeHtml(I18n.t('meeting.recent_title'))} <span class="counter">(${sessions.length})</span></h3>
            <div class="recent-list">
              ${recent || `<div class="meeting-empty-hint">${escapeHtml(I18n.t('meeting.no_history'))}</div>`}
            </div>
          </div>
        </div>
      </div>
    </tf-screen>`;
}

function renderJoiningScreen() {
  const failed = currentLifecycleStage === 'failed';
  const chipStatus = failed ? 'err' : 'warn';
  const chipLabel = failed
    ? I18n.t('meeting.status_error')
    : I18n.t('meeting.status_joining');
  const header = renderHeader(
    I18n.t('meeting.title'),
    I18n.t('meeting.joining_sub'),
    `<tf-chip status="${chipStatus}" dot>${escapeHtml(chipLabel)}</tf-chip>`,
    `<tf-button variant="danger" size="sm" icon="x" id="meeting-cancel-btn">${escapeHtml(I18n.t('meeting.cancel'))}</tf-button>`
  );

  // Resolve active step index. 'idle' → -1 (everything pending). 'failed' keeps
  // previous step as the failure point; we cannot tell which transition blew
  // up without more info, so we highlight the last known successful stage.
  const stageIndex = LIFECYCLE_STEPS.findIndex((s) => s.key === currentLifecycleStage);
  const stepsHtml = LIFECYCLE_STEPS.map((step, i) => {
    let cls = 'pending';
    let ico;
    if (failed) {
      if (i < stageIndex) { cls = 'done'; ico = sprite('check'); }
      else if (i === stageIndex) { cls = 'error'; ico = sprite('alert'); }
      else { cls = 'pending'; ico = String(i + 1); }
    } else if (stageIndex < 0) {
      ico = String(i + 1);
    } else if (i < stageIndex) {
      cls = 'done'; ico = sprite('check');
    } else if (i === stageIndex) {
      cls = 'active'; ico = String(i + 1);
    } else {
      ico = String(i + 1);
    }
    return `
      <div class="joining-step ${cls}">
        <div class="step-ico">${ico}</div>
        <div class="step-body">
          <div class="step-title">${escapeHtml(I18n.t(step.labelKey))}</div>
        </div>
      </div>`;
  }).join('');

  const failureNote = failed && currentLifecycleDetails
    ? `<div class="joining-failure">${escapeHtml(currentLifecycleDetails)}</div>`
    : '';
  const elapsed = formatElapsed(Date.now() - (joinStartedAt || Date.now()));

  return `
    <div class="meeting-joining-hero">
      <div class="meeting-joining-card">
        <tf-button class="meeting-joining-cancel" variant="ghost" size="sm" icon="x" id="meeting-cancel-btn" aria-label="${escapeAttr(I18n.t('meeting.cancel'))}"></tf-button>
        ${failed ? '' : '<div class="meeting-spinner"></div>'}
        <h2>${escapeHtml(I18n.t('meeting.joining_title'))}</h2>
        <p class="sub">${escapeHtml(activeSession?.meetingUrl || '')}</p>
        <div class="joining-elapsed">
          ${sprite('clock')}
          <span id="meeting-joining-elapsed">${escapeHtml(elapsed)}</span>
        </div>
        <div class="joining-steps">
          ${stepsHtml}
        </div>
        ${failureNote}
      </div>
    </div>`;
}

function measureRowHeight(t, listWidth) {
  const textWidth = Math.max(120, Math.floor(listWidth * T_TEXT_MAX_WIDTH));
  const txtH = measureItemHeight(t.text || ' ', {
    font: getDefaultFont(),
    maxWidth: textWidth,
    lineHeight: getDefaultLineHeight(),
  });
  // AI rows mają dashed border + padding 12+14 po obu stronach.
  const aiPad = ((t.speaker || '').toLowerCase() === 'tentaflow') ? 26 : 0;
  return Math.max(T_MIN_ROW, txtH + T_ROW_CHROME + aiPad);
}

function destroyHistoryVlist() {
  if (historyVlist) {
    try { historyVlist.destroy(); } catch {}
    historyVlist = null;
  }
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
  // Transcript tab — virtualizowana lista (vlist montowany po wstawieniu shell
  // do DOM, patrz bindEvents → history detail mount).
  const body =
    historyTab === 'summary'
      ? renderHistorySummary(historyDetail)
      : `<div class="transcript-list" id="meeting-history-transcript-list" style="height: 560px; overflow-y: auto;"></div>`;
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
      </div>
    </div>
    ${tabs}
    <div class="history-body">${body}</div>`;
}

function renderHistorySummary(_det) {
  // Summary dla historii bedzie listowane z meeting_summaries + meeting_action_items
  // przez nowe endpointy (Etap 2.2). Do tego czasu ekran pokazuje placeholder
  // zamiast fantomowego "empty summary".
  return `
    <div class="panel">
      <div class="panel-head">${sprite('sparkles')} ${escapeHtml(I18n.t('meeting.summary_pending_backend'))}</div>
    </div>`;
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
      : activeScreen === 'history'
      ? renderHistoryScreen()
      : activeScreen === 'settings'
      ? renderSettingsScreen()
      : activeScreen === 'error'
      ? renderErrorScreen()
      : renderJoinScreen();
  host.innerHTML = `<div class="meeting-app-root">${content}</div>`;
  if (activeScreen === 'history' && historyDetail && historyTab === 'transcript') {
    mountHistoryVlist(historyDetail.transcripts || []);
  } else {
    destroyHistoryVlist();
  }
  bindEvents();
}

function mountHistoryVlist(entries) {
  const host = byId('meeting-history-transcript-list');
  if (!host) return;
  destroyHistoryVlist();
  if (!entries.length) {
    host.innerHTML = `<div class="meeting-empty-hint" style="padding: 24px;">${escapeHtml(I18n.t('meeting.no_transcripts'))}</div>`;
    return;
  }
  historyListWidth = host.clientWidth || historyListWidth;
  historyVlist = createVirtualList(host, {
    items: entries,
    pinToBottom: false,
    overscan: 10,
    getItemHeight: (_i, e) => measureRowHeight(e, historyListWidth),
    renderItem: (_i, e) => renderTranscriptRow(e),
  });
}

function bindEvents() {
  byId('meeting-join-btn')?.addEventListener('click', onJoinClick);
  byId('meeting-cancel-btn')?.addEventListener('click', onCancelJoining);
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
    cleanupJoiningWatchers();
    activeScreen = activeSession ? 'active' : 'join';
    render();
  });
  byId('mt-retry')?.addEventListener('click', () => {
    cleanupJoiningWatchers();
    activeScreen = 'join';
    errorMessage = '';
    render();
  });
  byId('mt-save-settings')?.addEventListener('click', onSaveSettings);
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
  // History detail tabs
  document.querySelectorAll('[data-history-tab]').forEach((el) => {
    el.addEventListener('click', () => {
      if (historyTab === el.dataset.historyTab) return;
      historyTab = el.dataset.historyTab;
      render();
    });
  });
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
    await loadSettings();
    const existing = await fetchActiveSession();
    if (existing?.meetingKey) {
      activeSession = existing;
      await navigateToLive(existing.meetingKey);
      return;
    }
    await loadSessions();
    render();
    sessionListTimer = setInterval(loadSessions, 15000);
  },
  unmount() {
    destroyHistoryVlist();
    cleanupJoiningWatchers();
    if (sessionListTimer) {
      clearInterval(sessionListTimer);
      sessionListTimer = null;
    }
    activeSession = null;
    historyDetail = null;
  },
};

export default MeetingScreen;
