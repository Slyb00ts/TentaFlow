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

function newConversation(title) {
  const id = `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
  return {
    id,
    title: title || I18n.t('chat.new_conversation') || 'Nowa rozmowa',
    createdAt: Date.now(),
    updatedAt: Date.now(),
    messages: [],
  };
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
  const cls = `conv-item${conv.id === activeConvId ? ' active' : ''}`;
  return `
    <div class="${cls}" data-conv-id="${escapeHtml(conv.id)}">
      <span class="conv-title">${escapeHtml(conv.title)}</span>
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

function renderBubble(msg) {
  const isUser = msg.role === 'user';
  const isSystem = msg.role === 'system';
  const cls = isUser ? 'user' : (isSystem ? 'system' : 'assistant');
  const isStreaming = msg.streaming === true;

  const bubbleHtml = isUser
    ? escapeHtml(msg.text || '').replaceAll('\n', '<br>')
    : renderMarkdown(msg.text || '', { streaming: isStreaming });
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
  if (!isUser) {
    const fenceMatches = text.match(/```/g) || [];
    extra += Math.floor(fenceMatches.length / 2) * FENCE_HEADER_PX;
    const thinkMatches = text.match(/<think(?:ing)?>/gi) || [];
    extra += thinkMatches.length * THINK_COLLAPSED_PX;
  }

  const txtHeight = measureBubbleHeight(text, bubbleMax);
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

function mountVList() {
  const host = byId('chat-body');
  if (!host) return;
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

// ---- Conversation switching ----------------------------------------------

function selectConversation(id) {
  if (unsubscribe) { unsubscribe(); unsubscribe = null; }
  activeConvId = id;
  renderConvList();
  updateHeaderTitle();
  mountVList();
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
  mountVList();
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
  const modelSel = byId('chat-model');
  const modelId = modelSel?.value || (modelOptions[0]?.id ?? 'default');
  const userMessage = currentInputValue().trim();
  if (!userMessage) return;

  const conv = ensureActiveConv();
  setInputValue('');
  updateInputCounter();
  if (unsubscribe) { unsubscribe(); unsubscribe = null; }

  if (!conv.messages.length && (conv.title === 'Nowa rozmowa' || conv.title === (I18n.t('chat.new_conversation') || 'Nowa rozmowa'))) {
    conv.title = userMessage.slice(0, 40) + (userMessage.length > 40 ? '…' : '');
  }

  pushMessage(conv, { id: nextMsgId++, role: 'user', text: userMessage, ts: Date.now() });

  const modelLabel = currentModelLabel();
  const assistantMsg = { id: nextMsgId++, role: 'assistant', text: '', ts: Date.now(), streaming: true, modelLabel };
  pushMessage(conv, assistantMsg);

  ApiBinary.subscribe(
    'chatStreamRequest',
    { modelId, userMessage },
    {
      onChunk: (body) => {
        if (body.variant === 'ChatStreamChunk') {
          assistantMsg.text += body.delta;
          conv.updatedAt = Date.now();
          onStreamTick();
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
      },
      onError: (err) => {
        assistantMsg.streaming = false;
        assistantMsg.text = `[error] ${err.message ?? 'stream error'}`;
        toast(`${I18n.t('common.error')}: ${err.message ?? 'stream error'}`, 'error');
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
  mountVList();
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
  renderConvList();
  updateHeaderTitle();
  mountVList();
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
    activeConvId = conversations.length ? conversations.sort((a, b) => b.updatedAt - a.updatedAt)[0].id : null;
    let maxId = 0;
    for (const c of conversations) for (const m of c.messages) if (m.id > maxId) maxId = m.id;
    nextMsgId = maxId + 1;

    try {
      // Binary RPC `ModelListRequest` is the unified surface fed by services +
      // model_registry. Chat only routes "chat" capable models; whisper /
      // xtts rows would otherwise crash dispatch with "model not found in
      // configuration".
      const all = await ApiBinary.list('modelListRequest', { arrayKey: 'models' });
      const list = Array.isArray(all) ? all : [];
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
    } catch {
      modelOptions = [];
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
    mountVList();
    updateInputCounter();

    byId('chat-search')?.addEventListener('search', (e) => {
      searchFilter = e.detail.value || '';
      renderConvList();
    });

    byId('chat-new')?.addEventListener('click', () => {
      const conv = newConversation();
      conversations.push(conv);
      activeConvId = conv.id;
      saveConversations();
      renderConvList();
      updateHeaderTitle();
      mountVList();
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
    if (resizeListener) {
      window.removeEventListener('resize', resizeListener);
      resizeListener = null;
    }
  },
};

export default ChatScreen;
