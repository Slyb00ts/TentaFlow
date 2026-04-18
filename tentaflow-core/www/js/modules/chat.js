// =============================================================================
// Plik: modules/chat.js
// Opis: Streaming chat (R-STREAM) z ChatStreamRequest. Subskrypcja chunków,
//       wypisywanie incrementally, end z usage stats.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

let unsubscribe = null;
let modelOptions = [];

const ChatScreen = {
  title: 'Chat',
  render() {
    return `
      <div class="content-header"><h1>Chat</h1></div>
      <div class="card">
        <div class="form-row">
          <label class="label" for="chat-model">Model</label>
          <select class="select" id="chat-model"></select>
        </div>
        <div class="form-row">
          <label class="label" for="chat-input">Wiadomość</label>
          <textarea class="textarea" id="chat-input" placeholder="Wpisz wiadomość…" rows="3"></textarea>
        </div>
        <button class="btn btn-primary" id="chat-send">Wyślij</button>
        <div style="margin-top: var(--space-5);">
          <div class="label">Odpowiedź</div>
          <pre id="chat-output" style="background: var(--color-bg); padding: var(--space-4); border-radius: var(--radius-md); border: 1px solid var(--color-border); min-height: 120px; white-space: pre-wrap;"></pre>
          <div id="chat-stats" style="margin-top: var(--space-2); color: var(--color-text-muted); font-size: var(--text-xs);"></div>
        </div>
      </div>`;
  },
  async mount() {
    try {
      modelOptions = await ApiBinary.list('modelListRequest');
    } catch { modelOptions = []; }
    const sel = byId('chat-model');
    sel.innerHTML = modelOptions.length === 0
      ? `<option value="default">default</option>`
      : modelOptions.map((m) => `<option value="${escapeHtml(m.id)}">${escapeHtml(m.id)}</option>`).join('');
    byId('chat-send').addEventListener('click', sendMessage);
  },
  async unmount() {
    if (unsubscribe) { unsubscribe(); unsubscribe = null; }
  },
};

async function sendMessage() {
  const modelId = byId('chat-model').value;
  const userMessage = byId('chat-input').value.trim();
  if (!userMessage) return;

  if (unsubscribe) { unsubscribe(); unsubscribe = null; }

  const out = byId('chat-output');
  const stats = byId('chat-stats');
  out.textContent = '';
  stats.textContent = 'Generowanie…';

  try {
    unsubscribe = await ApiBinary.subscribe(
      'chatStreamRequest',
      { modelId, userMessage },
      {
        onChunk: (body) => {
          if (body.variant === 'ChatStreamChunk') {
            out.textContent += body.delta;
          }
        },
        onEnd: (body) => {
          if (body?.variant === 'ChatStreamEnd') {
            stats.textContent = `Tokeny: ${body.promptTokens} prompt + ${body.completionTokens} completion`;
          } else {
            stats.textContent = 'Zakończono';
          }
          unsubscribe = null;
        },
        onError: (err) => {
          stats.textContent = '';
          toast(`Błąd: ${err.message ?? 'stream error'}`, 'error');
          unsubscribe = null;
        },
      }
    );
  } catch (err) {
    toast(`Błąd: ${err.message}`, 'error');
  }
}

export default ChatScreen;
