// =============================================================================
// File: modules/chat.js — User-facing Chat app.
// Layout (matches wireframes-20260417 #chat-app):
//   [conversations sidebar 280px] | [model chip + status | virtualized body | composer]
// Virtualization: VirtualList + pretext text-measure. Handles 10k+ messages.
// Streaming: incremental tail-only height updates (O(1) per chunk).
// Conversations: persisted locally (localStorage) — server history API is
// a future addition; until then every user has a real, persistent local list.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { measureItemHeight, getDefaultFont, getDefaultLineHeight } from '/js/lib/text-measure.js';
import { createVirtualList } from '/js/lib/virtual-list.js';

const MAX_BUBBLE_WIDTH = 0.85;
const STORAGE_KEY = 'tentaflow_chat_conversations_v1';
const BUBBLE_CHROME_PX = 16 + 30 + 12; // bubble padding (20) + avatar gutter (30+12)

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
    if (!Array.isArray(parsed)) return [];
    return parsed;
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
    messages: [], // { id, role, text, ts }
  };
}

function activeConv() {
  return conversations.find((c) => c.id === activeConvId) || null;
}

// ---- Rendering helpers ---------------------------------------------------

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
  const text = last.text.replace(/\s+/g, ' ').trim();
  return prefix + (text.length > 60 ? `${text.slice(0, 60)}…` : text);
}

function renderConvItem(conv) {
  const cls = `conv-item${conv.id === activeConvId ? ' active' : ''}`;
  return `
    <div class="${cls}" data-conv-id="${escapeHtml(conv.id)}">
      <div class="conv-time">${escapeHtml(formatTime(conv.updatedAt))}</div>
      <div class="conv-title">${escapeHtml(conv.title)}</div>
      <div class="conv-snippet">${escapeHtml(lastMessagePreview(conv))}</div>
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
  host.innerHTML = filtered.map(renderConvItem).join('');
  host.querySelectorAll('.conv-item').forEach((el) => {
    el.addEventListener('click', () => {
      const id = el.dataset.convId;
      if (id && id !== activeConvId) {
        selectConversation(id);
      }
    });
  });
}

function renderBubble(msg) {
  const cls = msg.role === 'user' ? 'user' : (msg.role === 'system' ? 'system' : 'assistant');
  const isStreaming = msg.streaming === true;
  const avatar = msg.role === 'user'
    ? '<div class="avatar-mini">U</div>'
    : msg.role === 'assistant'
      ? '<div class="avatar-mini"><img src="/tentaflow.png" alt=""></div>'
      : '';
  const bubbleCls = isStreaming ? 'chat-bubble streaming' : 'chat-bubble';
  return `
    <div class="chat-msg chat-msg-${cls}">
      ${avatar}
      <div class="${bubbleCls}">${formatText(msg.text || '')}</div>
    </div>
  `;
}

function formatText(text) {
  return escapeHtml(text).replaceAll('\n', '<br>');
}

function itemHeight(msg) {
  // Bubble width: 85% of list width minus padding + avatar gutter.
  const maxBubbleWidth = Math.floor(listWidth * MAX_BUBBLE_WIDTH) - BUBBLE_CHROME_PX;
  const txtHeight = measureItemHeight(msg.text || ' ', {
    font: getDefaultFont(),
    maxWidth: Math.max(80, maxBubbleWidth),
    lineHeight: getDefaultLineHeight(),
  });
  // + bubble padding (20) + msg margin (16) + avatar alignment slack
  return Math.max(46, txtHeight + 36);
}

// ---- Virtual list mounting -----------------------------------------------

function mountVList() {
  const host = byId('chat-body');
  if (!host) return;
  listWidth = host.clientWidth;
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
  const w = host.clientWidth;
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
  if (!titleEl) return;
  const conv = activeConv();
  titleEl.textContent = conv ? conv.title : '';
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

function sendMessage() {
  const modelSel = byId('chat-model');
  const modelId = modelSel?.value || (modelOptions[0]?.id ?? 'default');
  const inputEl = byId('chat-input');
  const userMessage = (inputEl?.value || '').trim();
  if (!userMessage) return;

  const conv = ensureActiveConv();

  inputEl.value = '';
  if (unsubscribe) { unsubscribe(); unsubscribe = null; }

  // If conversation is still untitled, derive title from first user message.
  if (!conv.messages.length && (conv.title === 'Nowa rozmowa' || conv.title === (I18n.t('chat.new_conversation') || 'Nowa rozmowa'))) {
    conv.title = userMessage.slice(0, 40) + (userMessage.length > 40 ? '…' : '');
  }

  pushMessage(conv, { id: nextMsgId++, role: 'user', text: userMessage, ts: Date.now() });

  const assistantMsg = { id: nextMsgId++, role: 'assistant', text: '', ts: Date.now(), streaming: true };
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

function pushMessage(conv, msg) {
  // vlist.append pushuje element do items — a items to ta sama referencja co
  // conv.messages (przekazywana przez `items: messages` w mountVList).
  // Osobny `conv.messages.push(msg)` zdublowalby wpis. Fallback push tylko
  // gdy vlist nie jest jeszcze zainicjalizowany.
  if (vlist) {
    vlist.append(msg);
  } else {
    conv.messages.push(msg);
  }
  conv.updatedAt = Date.now();
  saveConversations();
}

// Wywolywane po kazdym chunk ze streama. Przy maly modelach LLM (vllm-metal
// Qwen 0.8B) tokeny lecą ~150-250/s — render viewport co chunk zacinal UI.
// Throttlujemy przez requestAnimationFrame do ~60fps: kazdy chunk modyfikuje
// assistantMsg.text, ale flush do DOM robimy raz na klatke. Delta tekstu
// juz jest w modelu wiec nic nie tracimy.
let streamRafPending = false;
function onStreamTick() {
  if (streamRafPending) return;
  streamRafPending = true;
  requestAnimationFrame(() => {
    streamRafPending = false;
    if (!vlist) return;
    const wasPinned = vlist.pinned;
    vlist.updateTail();
    const pill = byId('chat-new-pill');
    if (!pill) return;
    if (!wasPinned) {
      pill.classList.add('visible');
    } else {
      pill.classList.remove('visible');
    }
  });
}

// ---- Screen --------------------------------------------------------------

const ChatScreen = {
  get title() { return I18n.t('chat.title'); },

  render() {
    return `
      <div class="chat-shell">
        <aside class="chat-sidebar">
          <tf-searchbox id="chat-search" placeholder="${escapeHtml(I18n.t('chat.search_placeholder') || 'Szukaj rozmów...')}" debounce="200"></tf-searchbox>
          <div class="chat-new-btn">
            <tf-button variant="primary" icon="plus" id="chat-new">${escapeHtml(I18n.t('chat.new_conversation') || 'Nowa rozmowa')}</tf-button>
          </div>
          <div class="conv-list" id="chat-conv-list"></div>
        </aside>
        <section class="chat-main">
          <div class="chat-head">
            <tf-select class="chat-model-select" id="chat-model"></tf-select>
            <div class="status" id="chat-status">
              <span class="dot"></span>
              <span id="chat-head-title"></span>
            </div>
          </div>
          <div class="chat-body" id="chat-body"></div>
          <div class="chat-new-pill" id="chat-new-pill">${sprite('chevron-down')}${escapeHtml(I18n.t('chat.new_messages') || 'Nowe wiadomości')}</div>
          <div class="chat-input-row">
            <tf-textarea id="chat-input" autogrow rows="2"
              placeholder="${escapeHtml(I18n.t('chat.placeholder'))}"></tf-textarea>
            <tf-button variant="secondary" icon="paperclip" id="chat-attach" aria-label="${escapeHtml(I18n.t('chat.attach') || 'Załącz')}"></tf-button>
            <tf-button variant="primary" icon="send" id="chat-send">${escapeHtml(I18n.t('chat.send') || 'Wyślij')}</tf-button>
          </div>
        </section>
      </div>
    `;
  },

  async mount() {
    conversations = loadConversations();
    activeConvId = conversations.length ? conversations.sort((a, b) => b.updatedAt - a.updatedAt)[0].id : null;
    // Keep nextMsgId ahead of any persisted id.
    let maxId = 0;
    for (const c of conversations) for (const m of c.messages) if (m.id > maxId) maxId = m.id;
    nextMsgId = maxId + 1;

    try {
      modelOptions = await ApiBinary.list('modelListRequest');
    } catch {
      modelOptions = [];
    }

    const sel = byId('chat-model');
    const innerSelect = sel?.querySelector('select');
    // value = service id (do dispatchu), label = display_name (HF repo, np.
    // "Qwen/Qwen3.5-0.8B") gdy znany, fallback na id.
    const optionsHtml = modelOptions.length === 0
      ? `<option value="default">default</option>`
      : modelOptions.map((m) => {
          const label = m.display_name || m.displayName || m.id;
          return `<option value="${escapeHtml(m.id)}">${escapeHtml(label)}</option>`;
        }).join('');
    if (innerSelect) {
      innerSelect.innerHTML = optionsHtml;
      sel.setAttribute('value', innerSelect.value);
    }

    renderConvList();
    updateHeaderTitle();
    mountVList();

    // Search
    byId('chat-search')?.addEventListener('search', (e) => {
      searchFilter = e.detail.value || '';
      renderConvList();
    });

    // New conversation
    byId('chat-new')?.addEventListener('click', () => {
      const conv = newConversation();
      conversations.push(conv);
      activeConvId = conv.id;
      saveConversations();
      renderConvList();
      updateHeaderTitle();
      mountVList();
      byId('chat-input')?.focus();
    });

    // New-messages pill — click to scroll to bottom.
    byId('chat-new-pill')?.addEventListener('click', () => {
      vlist?.scrollToBottom();
      byId('chat-new-pill')?.classList.remove('visible');
    });

    // Send button
    byId('chat-send')?.addEventListener('click', sendMessage);

    // Attach — not wired yet (no backend endpoint). Honest toast instead of stub.
    byId('chat-attach')?.addEventListener('click', () => {
      toast(I18n.t('chat.attach_unavailable') || 'Załączniki wkrótce', 'info');
    });

    // Ctrl/Cmd+Enter submits; bare Enter inserts newline (multi-line composer).
    byId('chat-input')?.addEventListener('tf-keydown', (e) => {
      const { key, ctrlKey, metaKey } = e.detail;
      if (key === 'Enter' && (ctrlKey || metaKey)) {
        e.detail.original.preventDefault();
        sendMessage();
      }
    });

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
