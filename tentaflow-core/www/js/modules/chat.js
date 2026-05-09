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

const STORAGE_KEY = 'tentaflow_chat_conversations_v1';
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
let modelOptions = [];
let conversations = [];
let activeConvId = null;
let vlist = null;
let resizeListener = null;
let listWidth = 800;
let nextMsgId = 1;
let searchFilter = '';

// Tryb audio (Etap 1) — handle do FaceBackground.embed plus cache silnikow.
// faceHandle null gdy aktywna rozmowa jest w trybie tekstowym, niepusty gdy
// audio. engineCache wypelniany raz przy mount() z ApiBinary modelListRequest.
let faceHandle = null;
let engineCache = { stt: [], tts: [] };
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
  // STT/TTS engine wypelnia ensureAudioConfigDefaults() na bazie
  // /api/services/deployed. Pole `language` slesha jezyk transkrypcji per
  // konwersacja — defaultowo PL, w Etapie 2 caller (AudioPipeline) ustawi
  // wg `I18n.getLanguage()`.
  return {
    sttModel: 'whisper-1',
    ttsModel: 'tts-1',
    voice: 'nova',
    language: 'pl',
    sttEngine: null,
    ttsEngine: null,
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
          <div class="bubble">${bubbleHtml}${streamCaret}</div>
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
  // Bubble padding (24) + meta row (18) + row gap (20) + actions (28).
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

function renderAudioStage(conv) {
  const cfg = conv.audioConfig;
  const sttName = cfg.sttEngine || I18n.t('chat.audio_no_engine');
  const ttsName = cfg.ttsEngine || I18n.t('chat.audio_no_engine');
  const pendingTip = escapeHtml(I18n.t('chat.audio_pipeline_pending'));
  return `
    <div class="audio-stage" id="audio-stage" data-state="idle">
      <div class="audio-status" id="audio-status">
        <span class="dot"></span>
        <span class="label" id="audio-status-label">${escapeHtml(I18n.t('chat.audio_state_idle'))}</span>
        <span class="engine" id="audio-engine-name">—</span>
      </div>
      <div class="engine-pills">
        <button class="engine-pill" id="stt-pill" type="button" title="STT engine">
          <span class="lab">STT</span>
          <span class="name">${escapeHtml(sttName)}</span>
          <svg class="icon chev" aria-hidden="true"><use href="#i-chevron-down"/></svg>
        </button>
        <button class="engine-pill" id="tts-pill" type="button" title="TTS engine">
          <span class="lab">TTS</span>
          <span class="name">${escapeHtml(ttsName)}</span>
          <svg class="icon chev" aria-hidden="true"><use href="#i-chevron-down"/></svg>
        </button>
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

function engineLabel(engine) {
  return engine.display_name || engine.displayName || engine.id || engine.engine_id || engine.name || 'unknown';
}

function engineId(engine) {
  return engine.id || engine.engine_id || engine.name || 'unknown';
}

function ensureAudioConfigDefaults(conv) {
  // Wybiera deployed (stt|tts) engine zgodny z aktualnie zapisanym
  // sttEngine/ttsEngine, fallback na pierwszy z engineCache. Wartosci sa
  // walidowane wzgledem rzeczywistego registry — sztywne defaulty
  // 'whisper-1' / 'tts-1' (OpenAI compat) z newConversation() ZAWSZE
  // zamieniamy na real model_name, bo backend route_audio_transcription
  // /_speech rezolwuje przez `services_repo` i nie zna tych aliasow.
  const pickEngine = (kind) => {
    const cache = engineCache[kind];
    if (!cache.length) return null;
    const wanted = kind === 'stt' ? conv.audioConfig.sttEngine : conv.audioConfig.ttsEngine;
    if (wanted) {
      const found = cache.find((e) => engineId(e) === wanted);
      if (found) return found;
    }
    return cache[0];
  };
  const stt = pickEngine('stt');
  if (stt) {
    conv.audioConfig.sttEngine = engineId(stt);
    conv.audioConfig.sttModel = stt.model_name || stt.id || conv.audioConfig.sttModel;
  }
  const tts = pickEngine('tts');
  if (tts) {
    conv.audioConfig.ttsEngine = engineId(tts);
    conv.audioConfig.ttsModel = tts.model_name || tts.id || conv.audioConfig.ttsModel;
  }
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
    const preview = extractPlainText(m.text || '').slice(0, 200);
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

function updateEngineLabels() {
  const conv = activeConv();
  if (!conv) return;
  // Mapuje zapisany engine_id na display_name z aktualnego cache; gdy silnik
  // zniknal po deinstalacji pokazujemy ID jako fallback zamiast pustego pilla.
  const sttDisplay = (engineCache.stt.find((e) => engineId(e) === conv.audioConfig.sttEngine)
    && engineLabel(engineCache.stt.find((e) => engineId(e) === conv.audioConfig.sttEngine)))
    || conv.audioConfig.sttEngine;
  const ttsDisplay = (engineCache.tts.find((e) => engineId(e) === conv.audioConfig.ttsEngine)
    && engineLabel(engineCache.tts.find((e) => engineId(e) === conv.audioConfig.ttsEngine)))
    || conv.audioConfig.ttsEngine;
  const sttPillName = byId('stt-pill')?.querySelector('.name');
  const ttsPillName = byId('tts-pill')?.querySelector('.name');
  if (sttPillName) sttPillName.textContent = sttDisplay || I18n.t('chat.audio_no_engine');
  if (ttsPillName) ttsPillName.textContent = ttsDisplay || I18n.t('chat.audio_no_engine');
  const eng = byId('audio-engine-name');
  if (eng) eng.textContent = sttDisplay || '—';
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

// Otwiera prosty picker silnika — uzywa native <dialog>-style listy w
// kontekscie pill'a. tf-menu wymaga ze dzieci sa staticznie zadeklarowane,
// wiec zamiast tego budujemy ad-hoc menu w light DOM przy pillu. Wybor
// utrwala sie w conv.audioConfig i odswieza pill label.
function openEnginePicker(kind) {
  const list = engineCache[kind];
  if (!list || list.length === 0) {
    toast(I18n.t('chat.audio_engine_missing'), 'warning');
    return;
  }
  const conv = activeConv();
  if (!conv) return;
  const pill = byId(`${kind}-pill`);
  if (!pill) return;

  // Usun ewentualnie poprzednie ad-hoc menu (np. drugi klik w ten sam pill).
  pill.querySelector('.engine-pill-menu')?.remove();

  const menu = document.createElement('div');
  menu.className = 'engine-pill-menu';
  menu.setAttribute('role', 'menu');
  menu.innerHTML = list.map((e) => {
    const id = engineId(e);
    const label = engineLabel(e);
    return `<button type="button" role="menuitem" data-engine-id="${escapeHtml(id)}">${escapeHtml(label)}</button>`;
  }).join('');
  pill.appendChild(menu);

  const closeMenu = () => {
    menu.remove();
    document.removeEventListener('pointerdown', onDocDown, true);
  };
  function onDocDown(ev) {
    if (!menu.contains(ev.target) && ev.target !== pill && !pill.contains(ev.target)) {
      closeMenu();
    }
  }
  document.addEventListener('pointerdown', onDocDown, true);
  menu.addEventListener('click', (e) => {
    const btn = e.target.closest('button[data-engine-id]');
    if (!btn) return;
    const id = btn.dataset.engineId;
    const cache = engineCache[kind] || [];
    const picked = cache.find((e) => engineId(e) === id);
    if (kind === 'stt') {
      conv.audioConfig.sttEngine = id;
      if (picked) conv.audioConfig.sttModel = picked.model_name || picked.id || conv.audioConfig.sttModel;
    } else {
      conv.audioConfig.ttsEngine = id;
      if (picked) conv.audioConfig.ttsModel = picked.model_name || picked.id || conv.audioConfig.ttsModel;
    }
    saveConversations();
    updateEngineLabels();
    closeMenu();
  });
}

function bindAudioStageHandlers() {
  byId('audio-exit')?.addEventListener('click', () => switchMode('text'));
  byId('stt-pill')?.addEventListener('click', () => openEnginePicker('stt'));
  byId('tts-pill')?.addEventListener('click', () => openEnginePicker('tts'));

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

  if (targetMode === 'audio') {
    if (engineCache.stt.length === 0 || engineCache.tts.length === 0) {
      toast(I18n.t('chat.audio_engine_missing'), 'warning');
      return;
    }
    ensureAudioConfigDefaults(conv);
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
  if (conv.mode === 'audio') {
    if (vlist) { vlist.destroy(); vlist = null; }
    if (unsubscribe) { unsubscribe(); unsubscribe = null; }
    body.classList.add('audio-mode');
    body.innerHTML = renderAudioStage(conv);
    bindAudioStageHandlers();
    mountFace();
    renderRail();
    updateAudioStatus('idle');
    updateEngineLabels();
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

function selectConversation(id) {
  if (unsubscribe) { unsubscribe(); unsubscribe = null; }
  // Switch konwersacji = inny audioConfig + inny conv ref → pipeline z poprzedniej
  // rozmowy nie pasuje. Zatrzymujemy bezwarunkowo, applyMode() w docelowym mode
  // ponownie udostepni mic-button.
  stopAudioPipeline();
  activeConvId = id;
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
  const modelSel = byId('chat-model');
  const modelId = modelSel?.value || (modelOptions[0]?.id ?? 'default');
  const conv = ensureActiveConv();
  if (unsubscribe) { unsubscribe(); unsubscribe = null; }

  if (!conv.messages.length && (conv.title === 'Nowa rozmowa' || conv.title === (I18n.t('chat.new_conversation') || 'Nowa rozmowa'))) {
    conv.title = text.slice(0, 40) + (text.length > 40 ? '…' : '');
  }

  pushMessage(conv, { id: nextMsgId++, role: 'user', text, ts: Date.now(), via: opts.source || 'text' });

  const modelLabel = currentModelLabel();
  const assistantMsg = { id: nextMsgId++, role: 'assistant', text: '', ts: Date.now(), streaming: true, modelLabel, via: opts.source || 'text' };
  pushMessage(conv, assistantMsg);

  const feedAudio = audioPipeline && conv.mode === 'audio';

  ApiBinary.subscribe(
    'chatStreamRequest',
    { modelId, userMessage: text },
    {
      onChunk: (body) => {
        if (body.variant === 'ChatStreamChunk') {
          assistantMsg.text += body.delta;
          conv.updatedAt = Date.now();
          onStreamTick();
          if (feedAudio) audioPipeline.feedAssistantDelta(body.delta);
        }
      },
      onEnd: () => {
        unsubscribe = null;
        assistantMsg.streaming = false;
        if (assistantMsg.text === '') {
          assistantMsg.text = I18n.t('chat.empty_response') || '(empty response)';
        }
        conv.updatedAt = Date.now();
        saveConversations();
        onStreamTick();
        renderConvList();
        updateHeaderTitle();
        if (feedAudio) audioPipeline.feedAssistantEnd();
        if (conv.mode === 'audio' && conv.id === activeConvId) renderRail();
      },
      onError: (err) => {
        assistantMsg.streaming = false;
        assistantMsg.text = `[error] ${err.message ?? 'stream error'}`;
        toast(`${I18n.t('common.error')}: ${err.message ?? 'stream error'}`, 'error');
        saveConversations();
        onStreamTick();
        unsubscribe = null;
        if (feedAudio) audioPipeline.feedAssistantError(err);
      },
    },
  ).then((unsub) => {
    unsubscribe = unsub;
  }).catch((err) => {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    if (feedAudio) audioPipeline.feedAssistantError(err);
  });
}

// ---- AudioPipeline plumbing ---------------------------------------------

async function startAudioPipeline() {
  if (audioPipeline) return;
  const conv = activeConv();
  if (!conv || conv.mode !== 'audio' || !faceHandle) return;
  // Re-validate audioConfig wzgledem aktualnego registry. Bez tego stary
  // wpis z localStorage z `sttModel: 'whisper-1'` (defaultem z
  // newConversation()) lecial do API i routing nie znajdowal serwisu.
  ensureAudioConfigDefaults(conv);
  // Jezyk transkrypcji bierzemy z aktywnego I18n — w Etapie 1 conv.audioConfig
  // mial sztywne 'pl', ale uzytkownik moze rozmawiac w innym jezyku.
  const lang = (I18n.getLanguage && I18n.getLanguage()) || conv.audioConfig.language || 'pl';
  conv.audioConfig.language = lang;
  try {
    audioPipeline = new AudioPipeline({
      conv,
      faceHandle,
      i18n: I18n,
      onUserUtterance: (text) => {
        if (!text || text.trim().length === 0) {
          toast(I18n.t('chat.audio_empty_transcript'), 'info');
          return;
        }
        sendMessageInternal(text, { source: 'voice' });
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

function currentModelLabel() {
  const sel = byId('chat-model');
  const id = sel?.value;
  const m = modelOptions.find((m) => m.id === id);
  return m?.display_name || m?.displayName || id || 'Model';
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
              <tf-select class="chat-model-select" id="chat-model"></tf-select>
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

    // Jeden round-trip do backendu po wszystkie zarejestrowane silniki/modele;
    // rozdzielamy lokalnie per service_type. Zrodlem prawdy jest ApiBinary
    // (rkyv binary protocol) — REST /api/services/deployed nie istnieje.
    try {
      // Binary RPC `modelListRequest` is the unified surface fed by services +
      // model_registry. We split locally per category; STT/TTS engines are
      // needed for the audio mode (chat-audio.js), chat only routes "chat"
      // capable models.
      const all = (await ApiBinary.list('modelListRequest', { arrayKey: 'models' })) || [];
      const list = Array.isArray(all) ? all : [];
      const catOf = (m) => (m.category || m.service_type || '').toLowerCase();

      // Filter by capabilities (more granular than category — embedding-only
      // LLM rows would otherwise leak into chat dispatch).
      const chatOnly = list.filter((m) => {
        const caps = Array.isArray(m.capabilities) ? m.capabilities : [];
        return caps.length === 0 || caps.includes('chat');
      });
      const counts = new Map();
      for (const m of chatOnly) {
        counts.set(m.model_name, (counts.get(m.model_name) || 0) + 1);
      }
      modelOptions = chatOnly.map((m) => {
        const baseLabel = m.display_name || m.model_name;
        const dup = counts.get(m.model_name) > 1;
        return {
          id: m.model_name,
          serviceId: m.service_id,
          engineId: m.engine_id || '',
          label: dup && m.engine_id ? `${baseLabel} (${m.engine_id})` : baseLabel,
        };
      });
      engineCache.stt = list.filter((m) => catOf(m) === 'stt');
      engineCache.tts = list.filter((m) => catOf(m) === 'tts');
    } catch {
      modelOptions = [];
      engineCache.stt = [];
      engineCache.tts = [];
    }

    const sel = byId('chat-model');
    const innerSelect = sel?.querySelector('select');
    const optionsHtml = modelOptions.length === 0
      ? `<option value="default">default</option>`
      : modelOptions.map((m) => {
          return `<option value="${escapeHtml(m.id)}">${escapeHtml(m.label)}</option>`;
        }).join('');
    if (innerSelect) {
      innerSelect.innerHTML = optionsHtml;
      sel.setAttribute('value', innerSelect.value);
    }

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
  },

  async unmount() {
    if (unsubscribe) { unsubscribe(); unsubscribe = null; }
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
