// =============================================================================
// Plik: modules/chat.js
// Opis: Streaming chat z historia wiadomosci. Lista wiadomosci wirtualizowana
//       (VirtualList + pretext text-measure) — plynnie obsluguje 10000+
//       wiadomosci. Pin-to-bottom auto-scroll przy nowych chunkach.
//       Layout: header (model select) + scrollowana lista + bottom composer.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { measureItemHeight, getDefaultFont, getDefaultLineHeight } from '/js/lib/text-measure.js';
import { createVirtualList } from '/js/lib/virtual-list.js';

const MAX_BUBBLE_WIDTH = 0.85; // 85% kontenera

let unsubscribe = null;
let modelOptions = [];
let messages = []; // { id, role: 'user'|'assistant'|'system', text, ts }
let vlist = null;
let resizeListener = null;
let listWidth = 800;

const ChatScreen = {
  get title() { return I18n.t('chat.title'); },
  render() {
    return `
      <div class="chat-shell">
        <div class="chat-header">
          <h1>${escapeHtml(I18n.t('chat.title'))}</h1>
          <select class="input chat-model-select" id="chat-model"></select>
        </div>
        <div class="chat-list" id="chat-list"></div>
        <div class="chat-composer">
          <textarea class="input chat-input" id="chat-input"
            placeholder="${escapeHtml(I18n.t('chat.placeholder'))}"
            rows="2"></textarea>
          <button class="btn btn-primary chat-send" id="chat-send" aria-label="Send">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="22" y1="2" x2="11" y2="13"/><polygon points="22 2 15 22 11 13 2 9 22 2"/></svg>
          </button>
        </div>
      </div>
    `;
  },
  async mount() {
    try {
      modelOptions = await ApiBinary.list('modelListRequest');
    } catch {
      modelOptions = [];
    }
    const sel = byId('chat-model');
    sel.innerHTML = modelOptions.length === 0
      ? `<option value="default">default</option>`
      : modelOptions.map((m) => `<option value="${escapeHtml(m.id)}">${escapeHtml(m.id)}</option>`).join('');

    const host = byId('chat-list');
    listWidth = host.clientWidth;
    vlist = createVirtualList(host, {
      items: messages,
      pinToBottom: true,
      getItemHeight: (i, msg) => itemHeight(msg),
      renderItem: (i, msg) => renderBubble(msg),
    });

    byId('chat-send')?.addEventListener('click', sendMessage);
    byId('chat-input')?.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        sendMessage();
      }
    });

    resizeListener = () => {
      const w = host.clientWidth;
      if (Math.abs(w - listWidth) > 1) {
        listWidth = w;
        vlist?.refresh();
      }
    };
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

// ---- Rendering ------------------------------------------------------------

function renderBubble(msg) {
  const cls = msg.role === 'user' ? 'user' : (msg.role === 'system' ? 'system' : 'assistant');
  return `
    <div class="chat-msg chat-msg-${cls}">
      <div class="chat-bubble">${formatText(msg.text || '')}</div>
    </div>
  `;
}

function formatText(text) {
  // Bezpieczny escape + zachowaj newlines.
  return escapeHtml(text).replaceAll('\n', '<br>');
}

function itemHeight(msg) {
  // Wysokosc bubble = pomierzona wysokosc tekstu + padding (12+12) + margin (6+6).
  const maxBubbleWidth = Math.floor(listWidth * MAX_BUBBLE_WIDTH) - 28; // -padding
  const txtHeight = measureItemHeight(msg.text || '', {
    font: getDefaultFont(),
    maxWidth: Math.max(80, maxBubbleWidth),
    lineHeight: getDefaultLineHeight(),
  });
  return txtHeight + 36; // 24 padding + 12 margin
}

// ---- Send / receive -------------------------------------------------------

let nextId = 1;

function sendMessage() {
  const modelId = byId('chat-model').value;
  const inputEl = byId('chat-input');
  const userMessage = inputEl.value.trim();
  if (!userMessage) return;

  inputEl.value = '';

  if (unsubscribe) { unsubscribe(); unsubscribe = null; }

  // Push user message
  pushMessage({ id: nextId++, role: 'user', text: userMessage, ts: Date.now() });

  // Push pusty assistant bubble — bedzie streamowany w niego
  const assistantMsg = { id: nextId++, role: 'assistant', text: '', ts: Date.now() };
  pushMessage(assistantMsg);

  // Subscribe
  ApiBinary.subscribe(
    'chatStreamRequest',
    { modelId, userMessage },
    {
      onChunk: (body) => {
        if (body.variant === 'ChatStreamChunk') {
          assistantMsg.text += body.delta;
          updateMessage(assistantMsg.id);
        }
      },
      onEnd: (body) => {
        unsubscribe = null;
        if (assistantMsg.text === '' && body?.variant !== 'ChatStreamEnd') {
          assistantMsg.text = '(empty response)';
          updateMessage(assistantMsg.id);
        }
      },
      onError: (err) => {
        toast(`${I18n.t('common.error')}: ${err.message ?? 'stream error'}`, 'error');
        assistantMsg.text = `[error] ${err.message ?? 'stream error'}`;
        updateMessage(assistantMsg.id);
        unsubscribe = null;
      },
    },
  ).then((unsub) => {
    unsubscribe = unsub;
  }).catch((err) => {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  });
}

function pushMessage(msg) {
  messages.push(msg);
  vlist?.append(msg);
}

function updateMessage(id) {
  // Streaming chunk — message content sie zmienil, wysokosc tez moze.
  // VirtualList.refresh() przelicza wszystkie heights + render.
  // Pin-to-bottom auto-scrolluje jesli user nie scrollnal w gore.
  vlist?.refresh();
}

export default ChatScreen;
