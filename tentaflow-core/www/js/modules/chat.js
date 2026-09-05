// =============================================================================
// File: modules/chat.js — User-facing Chat app.
// Layout (matches design chat-redesign-20260430):
//   [conversations sidebar 296px] | [model picker + title + actions |
//    centered max-800px virtualized body | composer pill]
// Virtualization: VirtualList mounted directly on .chat-body. The centered
// 800px column is achieved via `padding-inline: max(24px, calc((100% - 800px)/2))`
// on .chat-body so the vlist host stays full-width and the scrollbar sits at
// the viewport edge. Streaming uses incremental tail-only height updates (O(1)
// per chunk).
// Conversations: persisted locally (localStorage) — server history API is
// a future addition.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { measureItemHeight, getDefaultFont, getDefaultLineHeight } from '/js/lib/text-measure.js';
import { createVirtualList } from '/js/lib/virtual-list.js';
import { renderMarkdown, extractPlainText } from '/js/lib/md-lite.js';
import FaceBackground from '/js/modules/faceBackground.js';
import { AudioPipeline } from '/js/modules/chat-audio.js';
import '/js/components/tf-agent-activity.js';
import { attachAgentActivity } from '/js/lib/agent-activity-bridge.js';

const STORAGE_KEY = 'tentaflow_chat_conversations_v1';
// The seeded "Default Chat" flow (db/seed.rs) — the same id on every node.
// Chat has no flow picker: every turn, typed or spoken, runs on this one.
const DEFAULT_CHAT_FLOW_ID = '00000000-0000-4000-8000-000000000010';
const MAX_INPUT_CHARS = 4096;
// Bubble chrome (avatar 36 + gap 12 + bubble padding 16+16). User messages do
// not span the full inner column; assistant messages do. Heuristic — overscan
// in VirtualList absorbs small drift from <think>/code blocks.
const AVATAR_AND_GAP_PX = 36 + 12;
const BUBBLE_PADDING_PX = 16 + 16;
const USER_BUBBLE_MAX = 680;
const FENCE_HEADER_PX = 30;
const THINK_COLLAPSED_PX = 40;

let unsubscribe = null;
// Teardown for the per-session agent-activity run-events subscription. Re-bound
// on every conversation switch (each conversation is its own session scope).
let agentActivityTeardown = null;
let conversations = [];
let activeConvId = null;
let vlist = null;
let resizeListener = null;
let listWidth = 800;
let nextMsgId = 1;
let searchFilter = '';

// Tryb audio — handle do FaceBackground.embed. null gdy aktywna rozmowa jest
// w trybie tekstowym, niepusty gdy audio.
let faceHandle = null;
// The Default Chat flow row, resolved once at mount(). Models (LLM/STT/TTS)
// are picked INSIDE that flow in the Flow Builder, so the browser sends no
// model id at all — a flow with no models has to say so, not quietly answer on
// whatever model happens to be deployed first. null when the flow is missing.
let defaultChatFlow = null;
let escKeyHandler = null;

// AudioPipeline (Etap 2) — zywy obiekt tylko gdy aktywna konwersacja jest w
// trybie audio I uzytkownik kliknal mic (gesture-gate). null w pozostalych
// stanach. spaceHeldHandler trzymamy w globalu zeby unmount() mogl je
// odlaczyc razem z escKeyHandler.
let audioPipeline = null;
let spaceKeydownHandler = null;
let spaceKeyupHandler = null;
let spaceHeld = false;

// ---- Persistence ---------------------------------------------------------

function loadConversations() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function saveConversations() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(conversations));
  } catch {
    // Quota exceeded — drop oldest half, retry once.
    conversations = conversations.slice(-Math.ceil(conversations.length / 2));
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(conversations)); } catch { /* give up */ }
  }
}

function defaultAudioConfig() {
  // `language` is the per-conversation transcription language, overwritten
  // from I18n.getLanguage() when the pipeline starts.
  return {
    language: 'pl',
  };
}

function newConversation(title) {
  const id = `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
  return {
    id,
    title: title || I18n.t('chat.new_conversation') || 'Nowa rozmowa',
    createdAt: Date.now(),
    updatedAt: Date.now(),
    messages: [],
    mode: 'text',
    audioConfig: defaultAudioConfig(),
  };
}

// Migracja konwersacji wczytanych z localStorage (sprzed wprowadzenia
// trybu audio). In-place — wolane zaraz po loadConversations(). Bez bumpu
// klucza STORAGE_KEY zeby nie tracic istniejacych rozmow uzytkownika.
function migrateConversations(list) {
  for (const c of list) {
    if (typeof c.mode !== 'string') c.mode = 'text';
    if (!c.audioConfig || typeof c.audioConfig !== 'object') {
      c.audioConfig = defaultAudioConfig();
    }
  }
}

function activeConv() {
  return conversations.find((c) => c.id === activeConvId) || null;
}

// ---- Sidebar rendering ---------------------------------------------------

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function formatTime(ts) {
  const diff = Date.now() - ts;
  if (diff < 60_000) return 'teraz';
  if (diff < 3600_000) return `${Math.floor(diff / 60_000)} min`;
  if (diff < 86400_000) return `${Math.floor(diff / 3600_000)} h`;
  return `${Math.floor(diff / 86400_000)} d`;
}

function lastMessagePreview(conv) {
  const last = conv.messages[conv.messages.length - 1];
  if (!last) return '';
  const prefix = last.role === 'user' ? 'User: ' : last.role === 'assistant' ? 'AI: ' : '';
  const text = extractPlainText(last.text || '');
  return prefix + (text.length > 60 ? `${text.slice(0, 60)}…` : text);
}

// Group conversations into Today / Yesterday / Earlier buckets for sidebar.
function groupByDay(items) {
  const now = new Date();
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const startOfYesterday = startOfToday - 86_400_000;
  const today = [];
  const yesterday = [];
  const earlier = [];
  for (const c of items) {
    if (c.updatedAt >= startOfToday) today.push(c);
    else if (c.updatedAt >= startOfYesterday) yesterday.push(c);
    else earlier.push(c);
  }
  const groups = [];
  if (today.length) groups.push({ label: I18n.t('chat.day_today') || 'Dziś', items: today });
  if (yesterday.length) groups.push({ label: I18n.t('chat.day_yesterday') || 'Wczoraj', items: yesterday });
  if (earlier.length) groups.push({ label: I18n.t('chat.day_earlier') || 'Wcześniej', items: earlier });
  return groups;
}

function renderConvItem(conv) {
  const isActive = conv.id === activeConvId;
  const isAudioActive = conv.mode === 'audio' && isActive;
  let cls = 'conv-item';
  if (isActive) cls += ' active';
  if (isAudioActive) cls += ' audio-now';
  const liveDot = isAudioActive ? '<span class="live-dot" aria-hidden="true"></span>' : '';
  return `
    <div class="${cls}" data-conv-id="${escapeHtml(conv.id)}">
      <span class="conv-title">${liveDot}${escapeHtml(conv.title)}</span>
      <span class="conv-time">${escapeHtml(formatTime(conv.updatedAt))}</span>
      <span class="conv-snippet">${escapeHtml(lastMessagePreview(conv))}</span>
    </div>
  `;
}

function renderConvList() {
  const host = byId('chat-conv-list');
  if (!host) return;
  const filter = searchFilter.trim().toLowerCase();
  const filtered = filter
    ? conversations.filter((c) =>
        c.title.toLowerCase().includes(filter) ||
        lastMessagePreview(c).toLowerCase().includes(filter))
    : conversations.slice();
  filtered.sort((a, b) => b.updatedAt - a.updatedAt);
  if (filtered.length === 0) {
    host.innerHTML = `<div class="conv-empty">${escapeHtml(I18n.t('chat.no_conversations') || 'Brak rozmów')}</div>`;
    return;
  }
  const groups = groupByDay(filtered);
  host.innerHTML = groups
    .map((g) => `<div class="conv-day">${escapeHtml(g.label)}</div>${g.items.map(renderConvItem).join('')}`)
    .join('');
  host.querySelectorAll('.conv-item').forEach((el) => {
    el.addEventListener('click', () => {
      const id = el.dataset.convId;
      if (id && id !== activeConvId) selectConversation(id);
      // Close drawer on mobile pick.
      document.querySelector('.chat-shell')?.classList.remove('drawer-open');
    });
  });
}

// ---- Bubble rendering ----------------------------------------------------

// Persistowany stan rozwiniecia <think> blokow per (msgId, blockIdx). Mapa
// zyje przez cala sesje GUI — virtualizer re-renderuje bubble przy scrollu,
// bez tej mapy `<details>` traci `open` po wyjsciu z viewport. Klucz to
// `${msgId}-${blockIdx}`. Brak wpisu = uzyj defaultu (streaming -> open).
const thinkOpenState = new Map();

function getThinkOpenState(key) {
  if (!key) return undefined;
  return thinkOpenState.has(key) ? thinkOpenState.get(key) : undefined;
}

function renderBubble(msg) {
  const isUser = msg.role === 'user';
  const isSystem = msg.role === 'system';
  const cls = isUser ? 'user' : (isSystem ? 'system' : 'assistant');
  const isStreaming = msg.streaming === true;

  const bubbleHtml = isUser
    ? escapeHtml(msg.text || '').replaceAll('\n', '<br>')
    : renderMarkdown(msg.text || '', {
        streaming: isStreaming,
        thinkKeyPrefix: String(msg.id || ''),
        getThinkOpen: getThinkOpenState,
      });
  const streamCaret = isStreaming && !isUser ? '<span class="streaming-caret"></span>' : '';

  const avatar = isUser
    ? '<div class="avatar user">U</div>'
    : isSystem
      ? ''
      : `<div class="avatar assistant">${sprite('model')}</div>`;

  const timeStr = formatBubbleTime(msg.ts);
  const meta = isUser
    ? `<div class="bubble-meta"><span>${timeStr}</span><span class="who">${escapeHtml(I18n.t('chat.you') || 'Ty')}</span></div>`
    : `<div class="bubble-meta"><span class="who">${escapeHtml(msg.modelLabel || I18n.t('chat.assistant') || 'Asystent')}</span><span>·</span><span>${timeStr}</span></div>`;

  // Stopka metryk inferencji — tylko dla gotowych (nie-streaming) odpowiedzi
  // asystenta z dostepnymi liczbami z ChatStreamEnd.
  const perf = (!isUser && !isStreaming) ? renderPerfFooter(msg.perf) : '';
  // Live step of an answer still being generated ("narzędzie · search_web",
  // "Odpalam 3 agentów"). Only while streaming: once the turn settles the
  // timeline in the activity widget is the record, not this line.
  const statusRow = (!isUser && isStreaming && msg.status)
    ? `<div class="bubble-status" role="status" aria-live="polite">
        <span class="bubble-status-dot"></span>
        <span class="bubble-status-text">${escapeHtml(msg.status)}</span>
      </div>`
    : '';

  const actions = isUser ? renderUserActions() : renderAssistantActions();

  return `
    <div class="msg-row ${cls}" data-msg-id="${msg.id}">
      ${isUser ? `
        <div class="bubble-wrap">
          ${meta}
          <div class="bubble">${bubbleHtml}${streamCaret}</div>
          ${actions}
        </div>
        ${avatar}
      ` : `
        ${avatar}
        <div class="bubble-wrap">
          ${meta}
          ${statusRow}
          <div class="bubble">${bubbleHtml}${streamCaret}</div>
          ${perf}
          ${actions}
        </div>
      `}
    </div>
  `;
}

function formatBubbleTime(ts) {
  if (!ts) return '';
  const d = new Date(ts);
  const pad = (n) => String(n).padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

// Stopka z metrykami inferencji pod odpowiedzia asystenta. Zwraca pusty string
// gdy brak danych albo gdy wszystkie wartosci sa zerowe (nie ma czego pokazac).
function renderPerfFooter(perf) {
  if (!perf) return '';
  const completionTokens = Number(perf.completionTokens || 0);
  const decodeTps = Number(perf.decodeTps || 0);
  const ttftMs = Number(perf.ttftMs || 0);
  const prefillTps = Number(perf.prefillTps || 0);
  const totalMs = Number(perf.totalMs || 0);
  if (!completionTokens && !decodeTps && !ttftMs && !prefillTps && !totalMs) return '';

  const lblTok = escapeHtml(I18n.t('chat.perf_tokens') || 'tok');
  const lblDecode = escapeHtml(I18n.t('chat.perf_decode') || 'tok/s');
  const lblTtft = escapeHtml(I18n.t('chat.perf_ttft') || 'TTFT');
  const lblPrefill = escapeHtml(I18n.t('chat.perf_prefill') || 'prefill');
  const lblTotal = escapeHtml(I18n.t('chat.perf_total') || 'łącznie');

  const parts = [];
  if (completionTokens) parts.push(`${completionTokens} ${lblTok}`);
  if (decodeTps) parts.push(`${Math.round(decodeTps)} ${lblDecode}`);
  if (ttftMs) parts.push(`${lblTtft} ${Math.round(ttftMs)} ms`);
  if (prefillTps) parts.push(`${lblPrefill} ${Math.round(prefillTps)} ${lblDecode}`);
  if (totalMs) parts.push(`${lblTotal} ${(totalMs / 1000).toFixed(totalMs < 10000 ? 2 : 1)} s`);

  return `<div class="bubble-perf">${parts.join(' · ')}</div>`;
}

function renderUserActions() {
  return `
    <div class="msg-actions">
      <button type="button" class="msg-act" data-act="copy" title="${escapeHtml(I18n.t('chat.copy') || 'Kopiuj')}">${sprite('copy')}</button>
      <button type="button" class="msg-act" data-act="edit" title="${escapeHtml(I18n.t('chat.edit') || 'Edytuj')}">${sprite('edit')}</button>
    </div>
  `;
}

function renderAssistantActions() {
  return `
    <div class="msg-actions">
      <button type="button" class="msg-act" data-act="copy" title="${escapeHtml(I18n.t('chat.copy') || 'Kopiuj')}">${sprite('copy')}</button>
      <button type="button" class="msg-act" data-act="regenerate" title="${escapeHtml(I18n.t('chat.regenerate') || 'Regeneruj')}">${sprite('refresh')}</button>
    </div>
  `;
}

// ---- Height heuristics ---------------------------------------------------

function measureBubbleHeight(text, maxWidth) {
  const txtHeight = measureItemHeight(text || ' ', {
    font: getDefaultFont(),
    maxWidth: Math.max(80, maxWidth),
    lineHeight: getDefaultLineHeight(),
  });
  return txtHeight;
}

// itemHeight is a heuristic (overscan absorbs the drift). For assistant
// messages with code fences / <think> blocks, add fixed-cost extras instead
// of doing per-segment monospace measurement — good enough for the virtualizer.
function itemHeight(msg) {
  const innerWidth = listWidth || 800;
  const isUser = msg.role === 'user';
  const bubbleMax = isUser
    ? Math.min(USER_BUBBLE_MAX, innerWidth) - BUBBLE_PADDING_PX
    : (innerWidth - AVATAR_AND_GAP_PX - BUBBLE_PADDING_PX);
  const text = msg.text || '';

  let extra = 0;
  let measuredText = text;
  if (!isUser) {
    const fenceMatches = text.match(/```/g) || [];
    extra += Math.floor(fenceMatches.length / 2) * FENCE_HEADER_PX;
    // Thinking block jest collapsed w DOM (md-lite renderuje <details>), wiec
    // jego dlugosc tekstu NIE liczy sie do wysokosci bubble — zliczamy tylko
    // chip (THINK_COLLAPSED_PX). Detekcja implicit-open: jezeli widzimy
    // </think> bez wczesniejszego <think>, calosc PRZED tagiem to thinking.
    const closingRe = /<\/think(?:ing)?>/gi;
    const explicitRe = /<think(?:ing)?>([\s\S]*?)<\/think(?:ing)?>/gi;
    let stripped = text;
    let blockCount = 0;
    stripped = stripped.replace(explicitRe, () => {
      blockCount += 1;
      return '';
    });
    const closes = stripped.match(closingRe);
    if (closes && closes.length > 0) {
      const lastClose = stripped.search(/<\/think(?:ing)?>[^<]*$/i);
      if (lastClose >= 0) {
        const tagEnd = stripped.indexOf('>', lastClose) + 1;
        stripped = stripped.slice(tagEnd);
        blockCount += 1;
      }
    }
    extra += blockCount * THINK_COLLAPSED_PX;
    measuredText = stripped;
  }

  const txtHeight = measureBubbleHeight(measuredText, bubbleMax);
  // Estymata przybliża PEŁNY offsetHeight itemu: bubble padding (24) + meta row
  // (18) + actions (28) + 20px odstępu, który teraz jest realnym
  // padding-bottom na `.chat-body .vlist-item` (więc wchodzi do offsetHeight).
  // Dzięki temu estymata pierwszego renderu ≈ wartość zmierzona przez
  // measureRendered i widok nie skacze, zanim pomiar skoryguje cache.
  return Math.max(60, txtHeight + extra + 90);
}

// ---- Virtual list mounting -----------------------------------------------

// Inner column width (used by itemHeight) = host clientWidth minus left+right
// computed padding. Centered 800px column comes from `.chat-body` padding-inline.
function computeInnerWidth(host) {
  const cs = window.getComputedStyle(host);
  const pl = parseFloat(cs.paddingLeft) || 0;
  const pr = parseFloat(cs.paddingRight) || 0;
  return Math.max(80, host.clientWidth - pl - pr);
}

// Delegated `toggle` listener na <details data-think-key>. Mountowany raz
// per host przez `dataset.thinkToggleBound`, zeby remount listy nie podpinal
// drugiej kopii. `toggle` event bublu, capture=true zlapie go z dowolnego
// rozwinietego/zwinietego details w drzewie.
function ensureThinkToggleListener(host) {
  if (!host || host.dataset.thinkToggleBound === '1') return;
  host.dataset.thinkToggleBound = '1';
  host.addEventListener('toggle', (e) => {
    const det = e.target;
    if (!(det instanceof HTMLDetailsElement)) return;
    const key = det.getAttribute('data-think-key');
    if (!key) return;
    thinkOpenState.set(key, det.open);
  }, true);
}

function mountVList() {
  const host = byId('chat-body');
  if (!host) return;
  ensureThinkToggleListener(host);
  listWidth = computeInnerWidth(host);
  const conv = activeConv();
  const messages = conv ? conv.messages : [];
  if (vlist) { vlist.destroy(); vlist = null; }
  vlist = createVirtualList(host, {
    items: messages,
    pinToBottom: true,
    measureHeights: true,
    overscan: 10,
    getItemHeight: (_i, msg) => itemHeight(msg),
    renderItem: (_i, msg) => renderBubble(msg),
    onScroll: (_top, _dist, { pinned }) => {
      const pill = byId('chat-new-pill');
      if (!pill) return;
      if (pinned) pill.classList.remove('visible');
    },
  });
}

function remountIfWidthChanged() {
  const host = byId('chat-body');
  if (!host) return;
  const w = computeInnerWidth(host);
  if (Math.abs(w - listWidth) > 1) {
    listWidth = w;
    vlist?.refresh();
  }
}

// ---- Audio mode (Etap 1) -------------------------------------------------

// Statyczna lista 8 dotow rozlozonych po obwodzie face-canvas. delay rozsuniety
// rownomiernie 0..1.05s zeby pulse wygladal jak fala biegnaca dookola.
const AMP_DOT_COUNT = 8;
function renderAmpDots() {
  let html = '';
  for (let i = 0; i < AMP_DOT_COUNT; i++) {
    const angle = (360 / AMP_DOT_COUNT) * i;
    const delay = (i * 0.13).toFixed(2);
    html += `<div class="amp-dot" style="--angle:${angle}deg;--delay:${delay}s"></div>`;
  }
  return html;
}

// Statyczne 20 barow waveform — animacja CSS waveDance, fazy rozsuniete.
const WAVE_BAR_COUNT = 20;
function renderWaveBars() {
  let html = '';
  for (let i = 0; i < WAVE_BAR_COUNT; i++) {
    const delay = (i * 0.045).toFixed(3);
    html += `<div class="bar" style="animation-delay:${delay}s"></div>`;
  }
  return html;
}

function renderAudioStage() {
  // Tryb audio odpala Default Chat (z jego blokami STT/LLM/TTS i modelami) —
  // nie ma tu zadnego wyboru flow ani silnikow, bo flow juz je ma.
  const pendingTip = escapeHtml(I18n.t('chat.audio_pipeline_pending'));
  return `
    <div class="audio-stage" id="audio-stage" data-state="idle">
      <div class="audio-status" id="audio-status">
        <span class="dot"></span>
        <span class="label" id="audio-status-label">${escapeHtml(I18n.t('chat.audio_state_idle'))}</span>
        <span class="engine" id="audio-engine-name">${escapeHtml(defaultChatFlowLabel())}</span>
      </div>
      <aside class="rail" id="audio-rail">
        <div class="rail-title">${escapeHtml(I18n.t('chat.audio_recent_entries'))}</div>
      </aside>
      <div class="face-stage">
        <div class="face-canvas" id="chat-face-stage"></div>
        ${renderAmpDots()}
      </div>
      <div class="subtitle" id="audio-subtitle">
        <div class="who" id="audio-who"></div>
        <div class="text" id="audio-text">${escapeHtml(I18n.t('chat.audio_preview_hint'))}</div>
      </div>
      <div class="wave" id="audio-wave">${renderWaveBars()}</div>
      <div class="audio-controls">
        <tf-button variant="ghost" icon="volume" id="audio-volume" disabled
          aria-label="${pendingTip}" title="${pendingTip}"></tf-button>
        <tf-button variant="primary" icon="mic" id="audio-mic" disabled
          aria-label="${pendingTip}" title="${pendingTip}"></tf-button>
        <tf-button variant="ghost" icon="pause" id="audio-pause" disabled
          aria-label="${pendingTip}" title="${pendingTip}"></tf-button>
        <tf-button variant="ghost" icon="x" id="audio-exit"
          aria-label="${escapeHtml(I18n.t('chat.audio_exit'))}"
          title="${escapeHtml(I18n.t('chat.audio_exit'))}">${escapeHtml(I18n.t('chat.audio_exit'))}</tf-button>
      </div>
    </div>
  `;
}

// Name of the flow every turn runs on — shown in the audio status line.
// A dash when it is missing; the toast on entering audio mode carries the why.
function defaultChatFlowLabel() {
  return defaultChatFlow?.name || '—';
}

// The flow reports an unconfigured block as "<llm|stt|tts> adapter: no model".
// Chat no longer has a model picker, so the only actionable answer is: pick the
// models inside Default Chat. Any other error is passed through verbatim.
function chatFlowError(message) {
  const raw = message || 'stream error';
  return /no model/i.test(raw) ? I18n.t('chat.default_flow_no_models') : raw;
}

function renderRail() {
  const conv = activeConv();
  const rail = byId('audio-rail');
  if (!rail || !conv) return;
  const last = conv.messages.slice(-4);
  const titleHtml = `<div class="rail-title">${escapeHtml(I18n.t('chat.audio_recent_entries'))}</div>`;
  if (last.length === 0) {
    rail.innerHTML = titleHtml +
      `<div class="rail-msg" style="opacity:.6">${escapeHtml(I18n.t('chat.audio_no_history'))}</div>`;
    return;
  }
  const itemsHtml = last.map((m) => {
    const cls = m.role === 'user' ? 'user' : 'bot';
    const who = m.role === 'user'
      ? I18n.t('chat.you')
      : (m.modelLabel || I18n.t('chat.assistant'));
    const time = formatBubbleTime(m.ts);
    // Pelny tekst w railu prawego panelu — wczesniejszy slice(0,200) ucinal
    // dlugie odpowiedzi (audio czytalo calosc, a panel pokazywal tylko 200 zn.).
    const preview = extractPlainText(m.text || '');
    return `
      <div class="rail-msg ${cls}">
        <div class="who">${escapeHtml(who)} · ${escapeHtml(time)}</div>
        <div>${escapeHtml(preview)}</div>
      </div>
    `;
  }).join('');
  rail.innerHTML = titleHtml + itemsHtml;
}

function updateAudioStatus(stateName, text) {
  const stage = byId('audio-stage');
  if (stage) stage.dataset.state = stateName;
  const label = byId('audio-status-label');
  if (label) label.textContent = text || I18n.t(`chat.audio_state_${stateName}`);
}

function mountFace() {
  const stage = byId('chat-face-stage');
  if (!stage) return;
  if (faceHandle) faceHandle.destroy();
  faceHandle = FaceBackground.embed(stage);
  // Etap 1: tylko idle. Inne stany (listen/think/speak) czekaja na
  // AudioPipeline (Etap 2) — API juz gotowe pod przyszlego callera.
  faceHandle.setMode('idle');
}

function destroyFace() {
  if (faceHandle) {
    faceHandle.destroy();
    faceHandle = null;
  }
}

function bindAudioStageHandlers() {
  byId('audio-exit')?.addEventListener('click', () => switchMode('text'));

  byId('audio-mic')?.addEventListener('click', async () => {
    if (!audioPipeline) {
      // Pierwszy klik = startuje pipeline (wymagany user gesture dla
      // getUserMedia). enableAudioControls() wywolane dopiero po sukcesie.
      await startAudioPipeline();
      return;
    }
    // Pipeline aktywny — toggle mute na mikrofonie.
    const willMute = !audioPipeline.isMuted();
    audioPipeline.mute(willMute);
    setMicMutedVisual(willMute);
  });

  byId('audio-pause')?.addEventListener('click', () => {
    if (!audioPipeline) return;
    // "Przerwij" — abort aktywnego LLM/TTS, zostaje listening.
    audioPipeline.abort();
  });

  byId('audio-volume')?.addEventListener('click', () => {
    if (!audioPipeline) return;
    const muted = audioPipeline.toggleSpeaker();
    byId('audio-volume')?.classList.toggle('muted', muted);
    if (muted) toast(I18n.t('chat.audio_speaker_muted'), 'info');
  });
}

function setActiveModeToggle(mode) {
  const textBtn = byId('chat-mode-text');
  const audioBtn = byId('chat-mode-audio');
  const isAudio = mode === 'audio';
  textBtn?.classList.toggle('active', !isAudio);
  audioBtn?.classList.toggle('active', isAudio);
  // tf-button variant przelaczamy zeby aktywny mial primary look (tf-button
  // exposuje setAttribute variant). Pozwala uzyskac wizualny kontrast bez
  // walki z shadow-DOM stylowaniem od zewnatrz.
  if (textBtn) textBtn.setAttribute('variant', isAudio ? 'ghost' : 'primary');
  if (audioBtn) audioBtn.setAttribute('variant', isAudio ? 'primary' : 'ghost');
}

function switchMode(targetMode) {
  const conv = ensureActiveConv();
  if (!conv) return;
  if (conv.mode === targetMode) return;

  if (targetMode === 'audio' && !defaultChatFlow) {
    toast(I18n.t('chat.default_flow_missing'), 'error');
    return;
  }

  conv.mode = targetMode;
  conv.updatedAt = Date.now();
  saveConversations();
  applyMode(conv);
  renderConvList();
  updateHeaderTitle();
  setActiveModeToggle(targetMode);
}

// applyMode swapuje zawartosc #chat-body miedzy widokiem tekstowym a audio
// w zaleznosci od conv.mode. Wolane przez switchMode i selectConversation.
function applyMode(conv) {
  const body = byId('chat-body');
  if (!body) return;
  // The activity widget lives in the composer wrap (outside #chat-body), so it
  // survives mode swaps; only its density changes — audio mode uses the narrow
  // dot+badge variant (§3.9).
  const activity = byId('chat-agent-activity');
  if (activity) activity.variant = conv.mode === 'audio' ? 'chat-audio' : 'chat';
  if (conv.mode === 'audio') {
    if (vlist) { vlist.destroy(); vlist = null; }
    if (unsubscribe) { unsubscribe(); unsubscribe = null; }
    body.classList.add('audio-mode');
    body.innerHTML = renderAudioStage();
    bindAudioStageHandlers();
    mountFace();
    renderRail();
    updateAudioStatus('idle');
    // Mic enabled w trybie pre-gesture — czeka na klik aby uruchomic
    // AudioPipeline (getUserMedia wymaga user-gesture). Volume/Pause zostaja
    // disabled do momentu gdy pipeline ruszy.
    const mic = byId('audio-mic');
    if (mic) {
      mic.removeAttribute('disabled');
      mic.setAttribute('title', I18n.t('chat.audio_start_mic'));
    }
  } else {
    stopAudioPipeline();
    destroyFace();
    body.classList.remove('audio-mode');
    body.innerHTML = '';
    mountVList();
  }
}

// ---- Conversation switching ----------------------------------------------

// (Re)subscribe the agent-activity widget to the active conversation's session
// scope. The conversation id is the flow `session_id` background runs publish
// under (§3.9). Tears down any prior subscription first.
function rebindAgentActivity() {
  if (agentActivityTeardown) { agentActivityTeardown(); agentActivityTeardown = null; }
  const widget = byId('chat-agent-activity');
  if (!widget || !activeConvId) return;
  agentActivityTeardown = attachAgentActivity(widget, activeConvId, {
    // Mirror the live step into the answer that is still streaming, so a slow
    // local model shows "narzędzie · search_web" instead of an empty bubble.
    onStatus: (status) => {
      const conv = conversations.find((c) => c.id === activeConvId);
      const streamingMsg = conv?.messages?.find((m) => m.streaming);
      if (!streamingMsg || streamingMsg.status === status) return;
      streamingMsg.status = status;
      onStreamTick();
    },
  });
}

function selectConversation(id) {
  if (unsubscribe) { unsubscribe(); unsubscribe = null; }
  // Switch konwersacji = inny audioConfig + inny conv ref → pipeline z poprzedniej
  // rozmowy nie pasuje. Zatrzymujemy bezwarunkowo, applyMode() w docelowym mode
  // ponownie udostepni mic-button.
  stopAudioPipeline();
  activeConvId = id;
  rebindAgentActivity();
  renderConvList();
  updateHeaderTitle();
  const conv = activeConv();
  if (conv) {
    applyMode(conv);
    setActiveModeToggle(conv.mode);
  } else {
    mountVList();
  }
}

function updateHeaderTitle() {
  const titleEl = byId('chat-head-title');
  const metaEl = byId('chat-head-meta');
  const conv = activeConv();
  if (titleEl) titleEl.textContent = conv ? conv.title : '';
  if (metaEl) {
    const count = conv ? conv.messages.length : 0;
    const label = I18n.t('chat.connected') || 'Połączony';
    const msgsLabel = I18n.t('chat.messages_count') || 'wiadomości';
    metaEl.textContent = conv ? `${label} · ${count} ${msgsLabel}` : '';
  }
}

// ---- Send / receive ------------------------------------------------------

function ensureActiveConv() {
  if (activeConv()) return activeConv();
  const conv = newConversation();
  conversations.push(conv);
  activeConvId = conv.id;
  saveConversations();
  renderConvList();
  updateHeaderTitle();
  // Nowa konwersacja zawsze startuje w trybie tekstowym — bez specjalnej
  // sciezki audio (uzytkownik musi swiadomie kliknac toggle).
  mountVList();
  setActiveModeToggle('text');
  return conv;
}

function currentInputValue() {
  const inputEl = byId('chat-input');
  return inputEl?.value || '';
}

function setInputValue(value) {
  const inputEl = byId('chat-input');
  if (inputEl) inputEl.value = value;
}

function sendMessage() {
  const text = currentInputValue().trim();
  if (!text) return;
  setInputValue('');
  updateInputCounter();
  sendMessageInternal(text, { source: 'text' });
}

// sendMessageInternal — wspolna sciezka dla wiadomosci tekstowych (z input box)
// i glosowych (transkrybowanych przez AudioPipeline). opts.source pozwala
// callerowi rozroznic via=voice w meta wiadomosci, a zarazem decyduje
// czy assistant deltas trzeba feedowac do AudioPipeline.
function sendMessageInternal(text, opts = {}) {
  // Czat dziala WYLACZNIE przez Default Chat — zadnego wyboru flow w GUI i
  // zadnego fallbacku na surowy model. Brak tego flow = twardy stop.
  if (!defaultChatFlow) {
    toast(I18n.t('chat.default_flow_missing'), 'error');
    return;
  }
  const modelLabel = defaultChatFlow.name || 'Default Chat';
  const conv = ensureActiveConv();
  if (unsubscribe) { unsubscribe(); unsubscribe = null; }

  if (!conv.messages.length && (conv.title === 'Nowa rozmowa' || conv.title === (I18n.t('chat.new_conversation') || 'Nowa rozmowa'))) {
    conv.title = text.slice(0, 40) + (text.length > 40 ? '…' : '');
  }

  pushMessage(conv, { id: nextMsgId++, role: 'user', text, ts: Date.now(), via: opts.source || 'text' });

  const assistantMsg = { id: nextMsgId++, role: 'assistant', text: '', ts: Date.now(), streaming: true, modelLabel, via: opts.source || 'text' };
  pushMessage(conv, assistantMsg);

  // Typed messages always go through the text chatStreamRequest (the backend
  // sets outputAudio=false itself, so the flow's TTS block stays transparent).
  // Głosowe wypowiedzi idą osobno przez sendVoiceUtterance (FlowInvoke).
  ApiBinary.subscribe(
    'chatStreamRequest',
    // conv.id is the flow session id — lets conversation_history / memory nodes
    // in Default Chat key off the conversation (Agent flows require it).
    // modelId stays empty on purpose: the models come from the flow's blocks.
    { modelId: '', userMessage: text, flowId: defaultChatFlow.id, sessionId: conv.id },
    {
      onChunk: (body) => {
        if (body.variant === 'ChatStreamChunk') {
          assistantMsg.text += body.delta;
          conv.updatedAt = Date.now();
          onStreamTick();
        }
      },
      onEnd: (endBody) => {
        unsubscribe = null;
        assistantMsg.streaming = false;
        assistantMsg.status = '';
        // ChatStreamEnd.text = pelny zakumulowany tekst z serwera — uzyj go
        // gdy zlozone delty sa puste (np. zgubione chunki), zanim pokazemy
        // "(pusta odpowiedz)".
        if (assistantMsg.text === '' && endBody && typeof endBody.text === 'string' && endBody.text.length > 0) {
          assistantMsg.text = endBody.text;
        }
        if (assistantMsg.text === '') {
          assistantMsg.text = I18n.t('chat.empty_response') || '(empty response)';
        }
        // Metryki wydajnosci inferencji z ChatStreamEnd — backend podaje je w
        // obu konwencjach (camelCase + snake_case), wartosci to liczby JS.
        if (endBody) {
          assistantMsg.perf = {
            promptTokens: Number(endBody.promptTokens ?? endBody.prompt_tokens ?? 0),
            completionTokens: Number(endBody.completionTokens ?? endBody.completion_tokens ?? 0),
            ttftMs: Number(endBody.ttftMs ?? endBody.ttft_ms ?? 0),
            prefillTps: Number(endBody.prefillTps ?? endBody.prefill_tps ?? 0),
            decodeTps: Number(endBody.decodeTps ?? endBody.decode_tps ?? 0),
            totalMs: Number(endBody.totalMs ?? endBody.total_ms ?? 0),
          };
        }
        conv.updatedAt = Date.now();
        saveConversations();
        onStreamTick();
        renderConvList();
        updateHeaderTitle();
        if (conv.mode === 'audio' && conv.id === activeConvId) renderRail();
      },
      onError: (err) => {
        assistantMsg.streaming = false;
        assistantMsg.status = '';
        const detail = chatFlowError(err.message);
        assistantMsg.text = `[error] ${detail}`;
        toast(`${I18n.t('common.error')}: ${detail}`, 'error');
        saveConversations();
        onStreamTick();
        unsubscribe = null;
      },
    },
  ).then((unsub) => {
    unsubscribe = unsub;
  }).catch((err) => {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  });
}

// ---- AudioPipeline plumbing ---------------------------------------------

// sendVoiceUtterance — głosowa wypowiedź idzie binarnym FlowInvoke do flow
// engine: audio → STT → LLM → TTS, a flow odsyła przeplatane tekst+audio.
// Tekst dopisujemy do bąbla asystenta, audio podajemy do AudioPipeline.
function sendVoiceUtterance(wav, sampleRate) {
  const conv = ensureActiveConv();
  if (unsubscribe) { unsubscribe(); unsubscribe = null; }

  // User bubble starts as a mic placeholder; the flow's `transcript` chunk
  // (emitted before the first assistant token) replaces it with the STT text.
  const userMsg = { id: nextMsgId++, role: 'user', text: '🎤', ts: Date.now(), via: 'voice' };
  pushMessage(conv, userMsg);

  const lang = conv.audioConfig?.language || (I18n.getLanguage && I18n.getLanguage()) || 'pl';

  // Glos idzie tym samym Default Chat co tekst — bez fallbacku model+chat.
  if (!defaultChatFlow) {
    toast(I18n.t('chat.default_flow_missing'), 'error');
    return;
  }
  const modelLabel = defaultChatFlow.name || 'Default Chat';
  const assistantMsg = { id: nextMsgId++, role: 'assistant', text: '', ts: Date.now(), streaming: true, modelLabel, via: 'voice' };
  pushMessage(conv, assistantMsg);

  ApiBinary.subscribe(
    'flowInvokeRequest',
    {
      // Same flow as text chat; outputAudio switches the TTS block on. The
      // STT/LLM/TTS models all come from the flow's own blocks.
      flowId: defaultChatFlow.id,
      model: '',
      serviceType: 'chat',
      mime: 'audio/wav',
      sampleRate,
      audio: wav,
      language: lang,
      sessionId: conv.id,
      outputAudio: true,
    },
    {
      onChunk: (body) => {
        if (body.variant !== 'FlowInvokeChunk') return;
        if (body.kind === 'transcript') {
          const t = (body.text || '').trim();
          if (t) {
            userMsg.text = t;
            conv.updatedAt = Date.now();
            // The user bubble is not the tail, so updateTail() cannot patch it.
            if (vlist) vlist.refresh();
          }
        } else if (body.kind === 'text') {
          assistantMsg.text += body.delta || '';
          conv.updatedAt = Date.now();
          onStreamTick();
        } else if (body.kind === 'audio') {
          if (audioPipeline) audioPipeline.playAudioChunk(body.bytes, body.mime);
        }
      },
      onEnd: (endBody) => {
        unsubscribe = null;
        assistantMsg.streaming = false;
        // FlowInvokeEnd niesie error gdy flow padł (np. brak flow 'voice',
        // STT/LLM error). Bez tego pusty wynik wyglądał jak '(pusta odpowiedź)'.
        if (endBody && endBody.error) {
          const detail = chatFlowError(endBody.error);
          assistantMsg.text = `[error] ${detail}`;
          toast(`${I18n.t('common.error')}: ${detail}`, 'error');
        } else {
          // Pelny tekst z serwera (FlowInvokeEnd.text) jest autorytatywny — sklejane
          // delty streamu bywaja uciete na koncu (audio leci dluzej niz dolecial tekst).
          if (endBody && typeof endBody.text === 'string' && endBody.text.length > 0) {
            assistantMsg.text = endBody.text;
          }
          if (assistantMsg.text === '') {
            assistantMsg.text = I18n.t('chat.empty_response') || '(empty response)';
          }
        }
        conv.updatedAt = Date.now();
        saveConversations();
        onStreamTick();
        renderConvList();
        updateHeaderTitle();
        if (audioPipeline) audioPipeline.finishResponse();
        if (conv.mode === 'audio' && conv.id === activeConvId) renderRail();
      },
      onError: (err) => {
        assistantMsg.streaming = false;
        const detail = chatFlowError(err.message ?? 'flow error');
        assistantMsg.text = `[error] ${detail}`;
        toast(`${I18n.t('common.error')}: ${detail}`, 'error');
        saveConversations();
        onStreamTick();
        unsubscribe = null;
        if (audioPipeline) audioPipeline.feedAssistantError(err);
      },
    },
  ).then((unsub) => {
    unsubscribe = unsub;
  }).catch((err) => {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    if (audioPipeline) audioPipeline.feedAssistantError(err);
  });
}

async function startAudioPipeline() {
  if (audioPipeline) return;
  const conv = activeConv();
  if (!conv || conv.mode !== 'audio' || !faceHandle) return;
  if (!defaultChatFlow) {
    toast(I18n.t('chat.default_flow_missing'), 'error');
    updateAudioStatus('idle');
    return;
  }
  // Jezyk transkrypcji bierzemy z aktywnego I18n — w Etapie 1 conv.audioConfig
  // mial sztywne 'pl', ale uzytkownik moze rozmawiac w innym jezyku.
  const lang = (I18n.getLanguage && I18n.getLanguage()) || conv.audioConfig.language || 'pl';
  conv.audioConfig.language = lang;
  try {
    audioPipeline = new AudioPipeline({
      conv,
      faceHandle,
      i18n: I18n,
      onUtteranceAudio: (wav, sampleRate) => {
        if (!wav || wav.length === 0) {
          toast(I18n.t('chat.audio_empty_transcript'), 'info');
          return;
        }
        sendVoiceUtterance(wav, sampleRate);
      },
      onStateChange: (state) => {
        // FSM AudioPipeline → state stage'u 'idle'/'listen'/'think'/'speak'.
        const map = { idle: 'idle', listening: 'listen', transcribing: 'think', thinking: 'think', speaking: 'speak', error: 'idle' };
        updateAudioStatus(map[state] || 'idle');
        // Rail moze odswiezac sie czesto — to tani re-render z 4 wpisow.
        if (conv.id === activeConvId) renderRail();
      },
      onError: (err) => {
        // Loguj + toast — pipeline sam wraca do listen.
        // eslint-disable-next-line no-console
        console.error('[audio]', err);
        toast(`${I18n.t('chat.audio_error')}: ${err.message || err.name || 'unknown'}`, 'error');
      },
      bargeInAbort: () => {
        // Wywolywane gdy AudioPipeline zatrzymuje aktywny TTS i chce ze
        // nasz LLM stream tez zostal anulowany. Mark assistant msg.
        if (unsubscribe) { unsubscribe(); unsubscribe = null; }
        const c = activeConv();
        if (!c) return;
        const last = c.messages[c.messages.length - 1];
        if (last && last.role === 'assistant' && last.streaming) {
          last.streaming = false;
          const tag = I18n.t('chat.audio_interrupted') || '[interrupted]';
          last.text = (last.text || '') + ' ' + tag;
          saveConversations();
          onStreamTick();
        }
      },
    });
    await audioPipeline.start();
    enableAudioControls(true);
  } catch (err) {
    audioPipeline = null;
    enableAudioControls(false);
    if (err && err.name === 'NotAllowedError') {
      toast(I18n.t('chat.audio_mic_denied'), 'error');
    } else if (err && err.name === 'NotFoundError') {
      toast(I18n.t('chat.audio_no_mic'), 'error');
    } else {
      toast(`${I18n.t('chat.audio_error')}: ${err.message || err.name || err}`, 'error');
    }
  }
}

function stopAudioPipeline() {
  if (!audioPipeline) return;
  try { audioPipeline.stop(); } catch { /* ignore */ }
  audioPipeline = null;
  enableAudioControls(false);
}

function enableAudioControls(enabled) {
  // Toggle disabled na mic/volume/pause razem z tooltip update'em. Mic ma
  // odrebny title w stanie "click to start" (przed startAudioPipeline) —
  // tym sterujemy w applyMode dla stanu pre-gesture.
  const ids = ['audio-mic', 'audio-volume', 'audio-pause'];
  const tip = enabled ? '' : escapeHtml(I18n.t('chat.audio_pipeline_pending'));
  for (const id of ids) {
    const el = byId(id);
    if (!el) continue;
    if (enabled) el.removeAttribute('disabled');
    else el.setAttribute('disabled', '');
    if (tip) el.setAttribute('title', tip);
    else el.removeAttribute('title');
  }
}

// Ikona muted — toggluje wizualnie button. tf-button nie expose'uje ikony do
// runtime change, ale klasa .muted na hostie zmieni opacity i kolor; ikona
// zostaje 'mic' (uzytkownik widzi po opacity ze mic jest off).
function setMicMutedVisual(muted) {
  const el = byId('audio-mic');
  if (!el) return;
  el.classList.toggle('muted', muted);
  el.setAttribute('title', muted ? I18n.t('chat.audio_unmute') : I18n.t('chat.audio_mute'));
}

function pushMessage(conv, msg) {
  // vlist.append shares the items reference with conv.messages (passed via
  // mountVList items: messages). A separate conv.messages.push would dupe.
  if (vlist) {
    vlist.append(msg);
  } else {
    conv.messages.push(msg);
  }
  conv.updatedAt = Date.now();
  saveConversations();
  // Audio mode trzyma rail z 4 ostatnimi repliki — odswiez gdy nowa
  // wiadomosc dochodzi w trakcie rozmowy.
  if (conv.mode === 'audio' && conv.id === activeConvId) {
    renderRail();
  }
}

// Direct call (no rAF) — background tabs throttle rAF to <1Hz in Chrome,
// which would stall token rendering when the user switches away.
function onStreamTick() {
  if (!vlist) return;
  const wasPinned = vlist.pinned;
  vlist.updateTail();
  const pill = byId('chat-new-pill');
  if (!pill) return;
  if (!wasPinned) pill.classList.add('visible');
  else pill.classList.remove('visible');
}

// ---- Composer hints ------------------------------------------------------

function updateInputCounter() {
  const counter = byId('chat-input-counter');
  if (!counter) return;
  const len = currentInputValue().length;
  counter.textContent = `${len} / ${MAX_INPUT_CHARS} znaków`;
  counter.classList.toggle('warn', len > MAX_INPUT_CHARS * 0.75);
}

// ---- Click delegation for in-bubble actions ------------------------------

function onBodyClick(e) {
  const copyBtn = e.target.closest('.copy-btn');
  if (copyBtn) {
    const encoded = copyBtn.dataset.code || '';
    let plain = '';
    try { plain = decodeURIComponent(escape(atob(encoded))); } catch { plain = ''; }
    if (plain) {
      navigator.clipboard?.writeText(plain).then(
        () => toast(I18n.t('chat.copied') || 'Skopiowano', 'info'),
        () => toast(I18n.t('chat.copy_failed') || 'Nie udało się skopiować', 'error'),
      );
    }
    return;
  }
  const act = e.target.closest('.msg-act');
  if (act) {
    const action = act.dataset.act;
    const row = act.closest('.msg-row');
    const msgId = Number(row?.dataset.msgId);
    if (action === 'copy') {
      const conv = activeConv();
      const msg = conv?.messages.find((m) => m.id === msgId);
      if (msg) {
        navigator.clipboard?.writeText(msg.text || '');
        toast(I18n.t('chat.copied') || 'Skopiowano', 'info');
      }
    } else {
      toast(I18n.t('chat.coming_soon') || 'Wkrótce', 'info');
    }
  }
}

// ---- Header actions ------------------------------------------------------

function exportActiveConversation() {
  const conv = activeConv();
  if (!conv) { toast(I18n.t('chat.no_conversations'), 'info'); return; }
  const payload = {
    id: conv.id,
    title: conv.title,
    createdAt: conv.createdAt,
    updatedAt: conv.updatedAt,
    messages: conv.messages.map((m) => ({ role: m.role, text: m.text, ts: m.ts })),
  };
  const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const dt = new Date();
  const yyyy = dt.getFullYear();
  const mm = String(dt.getMonth() + 1).padStart(2, '0');
  const dd = String(dt.getDate()).padStart(2, '0');
  const a = document.createElement('a');
  a.href = url;
  a.download = `tentaflow-chat-${conv.id}-${yyyy}-${mm}-${dd}.json`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // setTimeout so the download dialog has the URL when it pops, then revoke.
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  toast(I18n.t('chat.export_done'), 'info');
}

function conversationToMarkdown(conv) {
  const lines = [`# ${conv.title}`, ''];
  const youLabel = I18n.t('chat.you');
  const asstLabel = I18n.t('chat.assistant');
  for (const m of conv.messages) {
    const who = m.role === 'user' ? youLabel : (m.modelLabel || asstLabel);
    lines.push(`**${who}:**`, '', m.text || '', '');
  }
  return lines.join('\n');
}

async function shareActiveConversation() {
  const conv = activeConv();
  if (!conv) { toast(I18n.t('chat.no_conversations'), 'info'); return; }
  const md = conversationToMarkdown(conv);
  // navigator.share is gated to secure contexts on mobile and accepts only
  // a plain text payload here; clipboard is the desktop fallback.
  if (navigator.share) {
    try {
      await navigator.share({ title: conv.title, text: md });
      return;
    } catch (err) {
      // User cancelled or unsupported MIME — fall through to clipboard.
      if (err && err.name === 'AbortError') return;
    }
  }
  try {
    await navigator.clipboard.writeText(md);
    toast(I18n.t('chat.share_done'), 'info');
  } catch {
    toast(I18n.t('chat.share_failed'), 'error');
  }
}

function renameActiveConversation() {
  const conv = activeConv();
  if (!conv) return;
  // eslint-disable-next-line no-alert
  const next = window.prompt(I18n.t('chat.rename_prompt'), conv.title);
  if (next == null) return;
  const trimmed = next.trim();
  if (!trimmed || trimmed === conv.title) return;
  conv.title = trimmed.slice(0, 200);
  conv.updatedAt = Date.now();
  saveConversations();
  renderConvList();
  updateHeaderTitle();
}

function clearActiveConversation() {
  const conv = activeConv();
  if (!conv) return;
  // eslint-disable-next-line no-alert
  if (!window.confirm(I18n.t('chat.confirm_clear'))) return;
  conv.messages = [];
  conv.updatedAt = Date.now();
  saveConversations();
  if (conv.mode === 'audio') {
    renderRail();
  } else {
    mountVList();
  }
  renderConvList();
  updateHeaderTitle();
  toast(I18n.t('chat.clear_done'), 'info');
}

function deleteActiveConversation() {
  const conv = activeConv();
  if (!conv) return;
  // eslint-disable-next-line no-alert
  if (!window.confirm(I18n.t('chat.confirm_delete'))) return;
  conversations = conversations.filter((c) => c.id !== conv.id);
  activeConvId = conversations[0]?.id || null;
  saveConversations();
  // Po usunieciu rozmowy audio pipeline + face musza zniknac — applyMode
  // dla nowo aktywnej (lub czystego stanu) zalatwia obie sciezki.
  stopAudioPipeline();
  destroyFace();
  renderConvList();
  updateHeaderTitle();
  const next = activeConv();
  if (next) {
    applyMode(next);
    setActiveModeToggle(next.mode);
  } else {
    const body = byId('chat-body');
    if (body) {
      body.classList.remove('audio-mode');
      body.innerHTML = '';
    }
    mountVList();
    setActiveModeToggle('text');
  }
  toast(I18n.t('chat.delete_done'), 'info');
}

// ---- Screen --------------------------------------------------------------

const ChatScreen = {
  get title() { return I18n.t('chat.title'); },

  render() {
    return `
      <div class="chat-shell">
        <aside class="chat-sidebar">
          <div class="sidebar-head">
            <tf-searchbox id="chat-search" placeholder="${escapeHtml(I18n.t('chat.search_placeholder') || 'Szukaj rozmów...')}" debounce="200"></tf-searchbox>
            <div class="chat-new-btn">
              <tf-button variant="primary" icon="plus" id="chat-new">${escapeHtml(I18n.t('chat.new_conversation') || 'Nowa rozmowa')}</tf-button>
            </div>
          </div>
          <div class="conv-list" id="chat-conv-list"></div>
        </aside>
        <div class="chat-scrim" id="chat-scrim"></div>
        <section class="chat-main">
          <div class="chat-head">
            <div class="chat-head-left">
              <tf-button variant="ghost" icon="management" id="chat-burger" class="head-burger" aria-label="Menu"></tf-button>
            </div>
            <div class="head-title">
              <span class="title" id="chat-head-title"></span>
              <span class="meta">
                <span class="dot-status"></span>
                <span id="chat-head-meta"></span>
              </span>
            </div>
            <div class="head-actions">
              <div class="mode-toggle" role="tablist" aria-label="${escapeHtml(I18n.t('chat.title'))}">
                <tf-button variant="primary" icon="message" id="chat-mode-text" data-mode="text"
                  title="${escapeHtml(I18n.t('chat.mode_text'))}"
                  aria-label="${escapeHtml(I18n.t('chat.mode_text'))}">${escapeHtml(I18n.t('chat.mode_text'))}</tf-button>
                <tf-button variant="ghost" icon="mic" id="chat-mode-audio" data-mode="audio"
                  title="${escapeHtml(I18n.t('chat.mode_audio'))}"
                  aria-label="${escapeHtml(I18n.t('chat.mode_audio'))}">${escapeHtml(I18n.t('chat.mode_audio'))}</tf-button>
              </div>
              <tf-button variant="ghost" icon="download" id="chat-export" aria-label="${escapeHtml(I18n.t('chat.export'))}" title="${escapeHtml(I18n.t('chat.export'))}"></tf-button>
              <tf-button variant="ghost" icon="share" id="chat-share" aria-label="${escapeHtml(I18n.t('chat.share'))}" title="${escapeHtml(I18n.t('chat.share'))}"></tf-button>
              <div class="chat-more-wrap">
                <tf-button variant="ghost" icon="management" id="chat-more" aria-label="${escapeHtml(I18n.t('chat.more'))}" title="${escapeHtml(I18n.t('chat.more'))}"></tf-button>
                <tf-menu id="chat-more-menu" placement="bottom-end">
                  <tf-menu-item action="rename" icon="edit">${escapeHtml(I18n.t('chat.menu_rename'))}</tf-menu-item>
                  <tf-menu-item action="clear" icon="refresh">${escapeHtml(I18n.t('chat.menu_clear'))}</tf-menu-item>
                  <tf-menu-divider></tf-menu-divider>
                  <tf-menu-item action="delete" icon="trash" danger>${escapeHtml(I18n.t('chat.menu_delete'))}</tf-menu-item>
                </tf-menu>
              </div>
            </div>
          </div>
          <div class="chat-body" id="chat-body"></div>
          <div class="chat-new-pill" id="chat-new-pill">${sprite('chevron-down')}<span>${escapeHtml(I18n.t('chat.new_messages') || 'Nowe wiadomości')}</span></div>
          <div class="composer-wrap">
            <tf-agent-activity id="chat-agent-activity" variant="chat"></tf-agent-activity>
            <div class="composer">
              <tf-button variant="ghost" icon="paperclip" id="chat-attach" class="composer-attach" aria-label="${escapeHtml(I18n.t('chat.attach') || 'Załącz')}"></tf-button>
              <tf-textarea id="chat-input" autogrow rows="1"
                placeholder="${escapeHtml(I18n.t('chat.placeholder'))}"></tf-textarea>
              <tf-button variant="primary" icon="send" id="chat-send" class="composer-send" aria-label="${escapeHtml(I18n.t('chat.send') || 'Wyślij')}"></tf-button>
            </div>
            <div class="composer-hints">
              <span class="kbd"><kbd>Enter</kbd> ${escapeHtml(I18n.t('chat.hint_send') || 'wyślij')}</span>
              <span class="kbd"><kbd>Shift</kbd>+<kbd>Enter</kbd> ${escapeHtml(I18n.t('chat.hint_newline') || 'nowa linia')}</span>
              <span class="spacer"></span>
              <span class="counter" id="chat-input-counter">0 / ${MAX_INPUT_CHARS} znaków</span>
            </div>
          </div>
        </section>
      </div>
    `;
  },

  async mount() {
    conversations = loadConversations();
    migrateConversations(conversations);
    activeConvId = conversations.length ? conversations.sort((a, b) => b.updatedAt - a.updatedAt)[0].id : null;
    let maxId = 0;
    for (const c of conversations) for (const m of c.messages) if (m.id > maxId) maxId = m.id;
    nextMsgId = maxId + 1;

    // Chat runs on ONE flow — the seeded Default Chat. Resolve it by its stable
    // id; the `isDefault` flag is the fallback for an installation whose admin
    // moved the default onto another flow.
    try {
      const flows = (await ApiBinary.list('flowListRequest')) || [];
      defaultChatFlow = flows.find((f) => String(f.id) === DEFAULT_CHAT_FLOW_ID)
        || flows.find((f) => f.isDefault || f.is_default)
        || null;
    } catch {
      defaultChatFlow = null;
    }
    if (!defaultChatFlow) toast(I18n.t('chat.default_flow_missing'), 'error');

    renderConvList();
    updateHeaderTitle();
    const initialConv = activeConv();
    if (initialConv && initialConv.mode === 'audio') {
      // Restore audio mode po reloadzie — mountFace dziala dopiero po render(),
      // a render() juz sie wykonal gdy mount() jest wywolywany.
      applyMode(initialConv);
    } else {
      mountVList();
    }
    setActiveModeToggle(initialConv?.mode || 'text');
    updateInputCounter();

    byId('chat-mode-text')?.addEventListener('click', () => switchMode('text'));
    byId('chat-mode-audio')?.addEventListener('click', () => switchMode('audio'));

    // Esc w trybie audio wraca do tekstu — keyboard escape hatch dla
    // uzytkownikow ktorzy nie znajda przycisku 'Zakoncz rozmowe'.
    escKeyHandler = (e) => {
      if (e.key !== 'Escape') return;
      const conv = activeConv();
      if (conv?.mode === 'audio') {
        switchMode('text');
      }
    };
    document.addEventListener('keydown', escKeyHandler);

    // Push-to-talk — Spacja w trybie audio (poza textarea/input) jest
    // manualnym override VAD. Trzymanie = mowa (ignoruje threshold), puscic
    // = end-of-utterance natychmiast. Ulatwia testy i uzycie w halasliwym
    // otoczeniu gdzie adaptive threshold jest nieskuteczny.
    spaceKeydownHandler = (e) => {
      if (e.key !== ' ' && e.code !== 'Space') return;
      if (activeConv()?.mode !== 'audio') return;
      const tgt = e.target;
      if (tgt && (tgt.tagName === 'INPUT' || tgt.tagName === 'TEXTAREA' || tgt.isContentEditable)) return;
      if (spaceHeld) return;
      spaceHeld = true;
      if (audioPipeline) audioPipeline.pushToTalkStart();
      e.preventDefault();
    };
    spaceKeyupHandler = (e) => {
      if (e.key !== ' ' && e.code !== 'Space') return;
      if (!spaceHeld) return;
      spaceHeld = false;
      if (audioPipeline && activeConv()?.mode === 'audio') audioPipeline.pushToTalkEnd();
    };
    document.addEventListener('keydown', spaceKeydownHandler);
    document.addEventListener('keyup', spaceKeyupHandler);

    byId('chat-search')?.addEventListener('search', (e) => {
      searchFilter = e.detail.value || '';
      renderConvList();
    });

    byId('chat-new')?.addEventListener('click', () => {
      const conv = newConversation();
      conversations.push(conv);
      activeConvId = conv.id;
      saveConversations();
      rebindAgentActivity();
      // Nowa rozmowa = tryb tekstowy; jesli wczesniej byl mountowany face,
      // applyMode sprzata go i przywraca vlist.
      stopAudioPipeline();
      destroyFace();
      const body = byId('chat-body');
      if (body) body.classList.remove('audio-mode');
      renderConvList();
      updateHeaderTitle();
      mountVList();
      setActiveModeToggle('text');
      byId('chat-input')?.focus();
      document.querySelector('.chat-shell')?.classList.remove('drawer-open');
    });

    byId('chat-new-pill')?.addEventListener('click', () => {
      vlist?.scrollToBottom();
      byId('chat-new-pill')?.classList.remove('visible');
    });

    byId('chat-send')?.addEventListener('click', sendMessage);

    byId('chat-attach')?.addEventListener('click', () => {
      toast(I18n.t('chat.attach_unavailable') || 'Załączniki wkrótce', 'info');
    });

    byId('chat-export')?.addEventListener('click', exportActiveConversation);
    byId('chat-share')?.addEventListener('click', shareActiveConversation);

    // More button toggles a tf-menu sibling; the menu handles outside-click
    // dismissal itself, so we only need the toggle and the action router.
    byId('chat-more')?.addEventListener('click', (e) => {
      e.stopPropagation();
      byId('chat-more-menu')?.toggle();
    });
    byId('chat-more-menu')?.addEventListener('action', (e) => {
      const action = e.detail?.action;
      if (action === 'rename') renameActiveConversation();
      else if (action === 'clear') clearActiveConversation();
      else if (action === 'delete') deleteActiveConversation();
    });

    byId('chat-burger')?.addEventListener('click', () => {
      document.querySelector('.chat-shell')?.classList.toggle('drawer-open');
    });
    byId('chat-scrim')?.addEventListener('click', () => {
      document.querySelector('.chat-shell')?.classList.remove('drawer-open');
    });

    // Composer keymap: bare Enter sends, Shift/Alt+Enter inserts newline,
    // Cmd/Ctrl+Enter is kept as a power-user alias. IME composition passes
    // through untouched so CJK input does not trigger a send mid-compose.
    byId('chat-input')?.addEventListener('tf-keydown', (e) => {
      const { key, ctrlKey, metaKey, shiftKey, altKey, original } = e.detail;
      if (original?.isComposing) return;
      if (key !== 'Enter') return;
      if (shiftKey || altKey) return; // newline
      original?.preventDefault();
      sendMessage();
      // Cmd/Ctrl+Enter falls through here too — fine, also sends.
      void ctrlKey; void metaKey;
    });

    byId('chat-input')?.addEventListener('input', updateInputCounter);

    byId('chat-body')?.addEventListener('click', onBodyClick);

    resizeListener = () => remountIfWidthChanged();
    window.addEventListener('resize', resizeListener);

    // Subscribe the activity widget to the active conversation's session scope.
    rebindAgentActivity();
  },

  async unmount() {
    if (unsubscribe) { unsubscribe(); unsubscribe = null; }
    if (agentActivityTeardown) { agentActivityTeardown(); agentActivityTeardown = null; }
    if (vlist) { vlist.destroy(); vlist = null; }
    stopAudioPipeline();
    destroyFace();
    if (escKeyHandler) {
      document.removeEventListener('keydown', escKeyHandler);
      escKeyHandler = null;
    }
    if (spaceKeydownHandler) {
      document.removeEventListener('keydown', spaceKeydownHandler);
      spaceKeydownHandler = null;
    }
    if (spaceKeyupHandler) {
      document.removeEventListener('keyup', spaceKeyupHandler);
      spaceKeyupHandler = null;
    }
    if (resizeListener) {
      window.removeEventListener('resize', resizeListener);
      resizeListener = null;
    }
  },
};

export default ChatScreen;
