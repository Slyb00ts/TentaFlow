// =============================================================================
// Plik: modules/chat/Chat.js
// Opis: Moduł Playground - testowanie LLM, TTS, STT, Vision w dashboardzie.
// =============================================================================

const Chat = (() => {
  'use strict';

  // Stan prywatny
  let models = [];
  let flows = [];
  let selectedModel = '';
  let selectedFlow = '';
  let messages = [];
  let conversations = [];
  let currentConvId = null;
  let attachments = [];
  let ttsEnabled = false;
  let sttEnabled = false;
  let paramsVisible = false;
  let abortController = null;
  let isStreaming = false;
  let mediaRecorder = null;
  let audioChunks = [];

  // Parametry generowania
  let systemPrompt = 'You are a helpful assistant.';
  let temperature = 0.2;
  let maxTokens = 4096;
  let topP = 1.0;
  let ttsVoice = 'nova';
  let sttLanguage = 'pl';
  let sttModelLoaded = false;

  const STORAGE_KEY = 'tentaflow_chat_conversations';

  // ---------------------------------------------------------------------------
  // Renderowanie HTML widoku
  // ---------------------------------------------------------------------------

  function render() {
    return `
      <div class="chat-container">
        <aside class="chat-sidebar" id="chat-sidebar">
          <div class="chat-sidebar-header">
            <button id="chat-new-conv" class="btn-new-conv" data-i18n="playground.new_conversation">+ ${I18n.t('playground.new_conversation')}</button>
          </div>
          <div class="chat-sidebar-list" id="chat-sidebar-list"></div>
        </aside>

        <div class="chat-main">
          <button id="chat-sidebar-toggle" class="chat-sidebar-toggle" title="Menu">&#9776;</button>

          <div class="chat-toolbar">
            <div class="chat-toolbar-row">
              <label class="chat-toolbar-label">
                <span data-i18n="playground.model">${I18n.t('playground.model')}</span>
                <select id="chat-model-select" class="chat-select"></select>
              </label>
              <label class="chat-toolbar-label">
                <span data-i18n="playground.flow">${I18n.t('playground.flow')}</span>
                <select id="chat-flow-select" class="chat-select"></select>
              </label>
              <div class="chat-toolbar-separator"></div>
              <label class="chat-toolbar-check">
                <input type="checkbox" id="chat-tts-toggle"> <span data-i18n="playground.tts">${I18n.t('playground.tts')}</span>
              </label>
              <label class="chat-toolbar-check">
                <input type="checkbox" id="chat-stt-toggle"> <span data-i18n="playground.stt">${I18n.t('playground.stt')}</span>
              </label>
              <div class="chat-toolbar-separator"></div>
              <button id="chat-params-toggle" class="btn-icon" title="${I18n.t('playground.params')}" data-i18n-title="playground.params">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <circle cx="12" cy="12" r="3"></circle>
                  <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.32 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"></path>
                </svg>
              </button>
            </div>
          </div>

          <div id="chat-params-panel" class="chat-params" hidden>
            <div class="chat-params-grid">
              <div class="chat-params-field chat-params-full">
                <label for="chat-system-prompt" data-i18n="playground.system_prompt">${I18n.t('playground.system_prompt')}</label>
                <textarea id="chat-system-prompt" rows="3">${Utils.escapeHtml(systemPrompt)}</textarea>
              </div>
              <div class="chat-params-field">
                <label for="chat-temperature"><span data-i18n="playground.temperature">${I18n.t('playground.temperature')}</span>: <span id="chat-temp-value">${temperature}</span></label>
                <input type="range" id="chat-temperature" min="0" max="2" step="0.1" value="${temperature}">
              </div>
              <div class="chat-params-field">
                <label for="chat-max-tokens" data-i18n="playground.max_tokens">${I18n.t('playground.max_tokens')}</label>
                <input type="number" id="chat-max-tokens" value="${maxTokens}" min="1" max="128000">
              </div>
              <div class="chat-params-field">
                <label for="chat-top-p"><span data-i18n="playground.top_p">${I18n.t('playground.top_p')}</span>: <span id="chat-topp-value">${topP}</span></label>
                <input type="range" id="chat-top-p" min="0" max="1" step="0.05" value="${topP}">
              </div>
              <div class="chat-params-field">
                <label for="chat-voice" data-i18n="playground.voice">${I18n.t('playground.voice')}</label>
                <select id="chat-voice" class="chat-select">
                  <option value="alloy"${ttsVoice === 'alloy' ? ' selected' : ''}>alloy</option>
                  <option value="echo"${ttsVoice === 'echo' ? ' selected' : ''}>echo</option>
                  <option value="fable"${ttsVoice === 'fable' ? ' selected' : ''}>fable</option>
                  <option value="onyx"${ttsVoice === 'onyx' ? ' selected' : ''}>onyx</option>
                  <option value="nova"${ttsVoice === 'nova' ? ' selected' : ''}>nova</option>
                  <option value="shimmer"${ttsVoice === 'shimmer' ? ' selected' : ''}>shimmer</option>
                </select>
              </div>
              <div class="chat-params-field">
                <label for="chat-language" data-i18n="playground.language">${I18n.t('playground.language')}</label>
                <select id="chat-language" class="chat-select">
                  <option value="pl"${sttLanguage === 'pl' ? ' selected' : ''}>pl</option>
                  <option value="en"${sttLanguage === 'en' ? ' selected' : ''}>en</option>
                  <option value="de"${sttLanguage === 'de' ? ' selected' : ''}>de</option>
                  <option value="fr"${sttLanguage === 'fr' ? ' selected' : ''}>fr</option>
                  <option value="es"${sttLanguage === 'es' ? ' selected' : ''}>es</option>
                  <option value="it"${sttLanguage === 'it' ? ' selected' : ''}>it</option>
                  <option value="pt"${sttLanguage === 'pt' ? ' selected' : ''}>pt</option>
                  <option value="nl"${sttLanguage === 'nl' ? ' selected' : ''}>nl</option>
                  <option value="ru"${sttLanguage === 'ru' ? ' selected' : ''}>ru</option>
                  <option value="ja"${sttLanguage === 'ja' ? ' selected' : ''}>ja</option>
                  <option value="ko"${sttLanguage === 'ko' ? ' selected' : ''}>ko</option>
                  <option value="zh"${sttLanguage === 'zh' ? ' selected' : ''}>zh</option>
                </select>
              </div>
              <div class="chat-params-field chat-params-full" id="chat-stt-model-panel">
                <label>Whisper large-v3-turbo (1.6 GB)</label>
                <div style="display:flex;gap:8px;align-items:center">
                  <button id="chat-stt-load-btn" class="btn-sm" style="white-space:nowrap">
                    <span id="chat-stt-load-text">${I18n.t('playground.stt_load', 'Load')}</span>
                  </button>
                  <span id="chat-stt-status" style="font-size:12px;color:var(--text-muted)"></span>
                </div>
              </div>
            </div>
          </div>

          <div id="chat-messages" class="chat-messages"></div>

          <div class="chat-input-wrapper">
            <div id="chat-attachments" class="chat-attachments" hidden></div>
            <div class="chat-input-area">
              <button id="chat-mic-btn" class="btn-icon" title="${I18n.t('playground.mic_title')}" data-i18n-title="playground.mic_title">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"></path>
                  <path d="M19 10v2a7 7 0 0 1-14 0v-2"></path>
                  <line x1="12" y1="19" x2="12" y2="23"></line>
                  <line x1="8" y1="23" x2="16" y2="23"></line>
                </svg>
              </button>
              <button id="chat-attach-btn" class="btn-icon" title="${I18n.t('playground.attach_title')}" data-i18n-title="playground.attach_title">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"></path>
                </svg>
              </button>
              <textarea id="chat-input" class="chat-textarea" placeholder="${I18n.t('playground.input_placeholder')}" data-i18n-placeholder="playground.input_placeholder" rows="1"></textarea>
              <button id="chat-send-btn" class="btn-icon btn-send" title="${I18n.t('playground.send')}" data-i18n-title="playground.send">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
                  <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"></path>
                </svg>
              </button>
            </div>
          </div>
        </div>
      </div>
    `;
  }

  // ---------------------------------------------------------------------------
  // Montowanie - podpięcie zdarzeń, załadowanie danych
  // ---------------------------------------------------------------------------

  function mount() {
    loadModels();
    loadFlows();
    loadConversations();

    if (!currentConvId) {
      newConversation();
    }

    // Przycisk wyślij / przerwij
    const sendBtn = document.getElementById('chat-send-btn');
    if (sendBtn) {
      sendBtn.addEventListener('click', handleSendClick);
    }

    // Enter = wyślij, Shift+Enter = nowa linia
    const input = document.getElementById('chat-input');
    if (input) {
      input.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && !e.shiftKey) {
          e.preventDefault();
          handleSendClick();
        }
      });
      input.addEventListener('input', () => autoResize(input));
    }

    // Parametry toggle
    const paramsToggle = document.getElementById('chat-params-toggle');
    if (paramsToggle) {
      paramsToggle.addEventListener('click', toggleParams);
    }

    // Parametry - aktualizacja wartości
    bindParamListeners();
    checkSttModelStatus();

    // TTS / STT checkboxy
    const ttsToggle = document.getElementById('chat-tts-toggle');
    if (ttsToggle) {
      ttsToggle.addEventListener('change', () => { ttsEnabled = ttsToggle.checked; });
    }
    const sttToggle = document.getElementById('chat-stt-toggle');
    if (sttToggle) {
      sttToggle.addEventListener('change', () => { sttEnabled = sttToggle.checked; });
    }

    // Mikrofon
    const micBtn = document.getElementById('chat-mic-btn');
    if (micBtn) {
      micBtn.addEventListener('click', handleSTT);
    }

    const sttLoadBtn = document.getElementById('chat-stt-load-btn');
    if (sttLoadBtn) {
      sttLoadBtn.addEventListener('click', handleSttLoadModel);
    }

    // Załączniki
    const attachBtn = document.getElementById('chat-attach-btn');
    if (attachBtn) {
      attachBtn.addEventListener('click', handleFileAttach);
    }

    // Zmiana modelu
    const modelSelect = document.getElementById('chat-model-select');
    if (modelSelect) {
      modelSelect.addEventListener('change', () => { selectedModel = modelSelect.value; });
    }

    // Zmiana flow
    const flowSelect = document.getElementById('chat-flow-select');
    if (flowSelect) {
      flowSelect.addEventListener('change', () => { selectedFlow = flowSelect.value; });
    }

    // Nowa konwersacja
    const newConvBtn = document.getElementById('chat-new-conv');
    if (newConvBtn) {
      newConvBtn.addEventListener('click', () => { newConversation(); });
    }

    // Toggle sidebar na mobile
    const sidebarToggle = document.getElementById('chat-sidebar-toggle');
    if (sidebarToggle) {
      sidebarToggle.addEventListener('click', () => {
        const sidebar = document.getElementById('chat-sidebar');
        if (sidebar) sidebar.classList.toggle('open');
      });
    }
  }

  // ---------------------------------------------------------------------------
  // Odmontowanie
  // ---------------------------------------------------------------------------

  function unmount() {
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    if (mediaRecorder && mediaRecorder.state === 'recording') {
      mediaRecorder.stop();
    }
    mediaRecorder = null;
    isStreaming = false;
  }

  // ---------------------------------------------------------------------------
  // Ładowanie modeli i flows z API
  // ---------------------------------------------------------------------------

  async function loadModels() {
    try {
      // Rownolegle pobierz aliasy, unified models i zwykle modele
      const [aliasesData, unifiedData, modelsData] = await Promise.all([
        ApiClient.get('/api/model-aliases').catch(() => []),
        ApiClient.get('/api/models/unified').catch(() => []),
        ApiClient.get('/api/models?limit=200').catch(() => [])
      ]);

      models = modelsData || [];
      const aliases = aliasesData || [];
      const unified = unifiedData || [];

      const select = document.getElementById('chat-model-select');
      if (!select) return;

      let html = '';

      // Grupa: Aliasy
      if (aliases.length > 0) {
        html += `<optgroup label="${Utils.escapeAttr(I18n.t('playground.aliases'))}">`;
        html += aliases.map(a => {
          const target = a.target_model || '';
          const label = target ? `${a.alias} \u2192 ${target}` : a.alias;
          return `<option value="alias:${Utils.escapeAttr(a.alias)}">${Utils.escapeHtml(label)}</option>`;
        }).join('');
        html += '</optgroup>';
      }

      // Grupa: Modele (unified z info o nodach)
      if (unified.length > 0) {
        html += `<optgroup label="${Utils.escapeAttr(I18n.t('playground.models'))}">`;
        html += unified.map(m => {
          const count = (m.instances || []).length;
          const countLabel = count > 1 ? ` (${count}${I18n.t('playground.node_count')})` : '';
          return `<option value="model:${Utils.escapeAttr(m.model_name)}">${Utils.escapeHtml(m.model_name)}${countLabel}</option>`;
        }).join('');
        html += '</optgroup>';
      } else if (models.length > 0) {
        html += `<optgroup label="${Utils.escapeAttr(I18n.t('playground.models'))}">`;
        html += models.map(m =>
          `<option value="model:${Utils.escapeAttr(m.model_name)}">${Utils.escapeHtml(m.model_name)}</option>`
        ).join('');
        html += '</optgroup>';
      }

      // Grupy per node (z unified instances)
      const nodeMap = {};
      for (const m of unified) {
        for (const inst of (m.instances || [])) {
          const nodeName = inst.node_name || inst.node_id;
          if (!nodeName) continue;
          if (!nodeMap[nodeName]) nodeMap[nodeName] = [];
          nodeMap[nodeName].push({ model_name: m.model_name, node_id: inst.node_id });
        }
      }
      for (const [nodeName, nodeModels] of Object.entries(nodeMap)) {
        html += `<optgroup label="${Utils.escapeAttr(nodeName)}">`;
        html += nodeModels.map(nm =>
          `<option value="node:${Utils.escapeAttr(nm.node_id)}:${Utils.escapeAttr(nm.model_name)}">${Utils.escapeHtml(nm.model_name)}</option>`
        ).join('');
        html += '</optgroup>';
      }

      select.innerHTML = html;

      // Ustaw domyslny model
      if (select.options.length > 0 && !selectedModel) {
        selectedModel = select.options[0].value;
      }
    } catch (e) {
      console.error('Blad ladowania modeli:', e);
    }
  }

  async function loadFlows() {
    try {
      const data = await ApiClient.get('/api/flows?limit=100');
      flows = data || [];
      const select = document.getElementById('chat-flow-select');
      if (!select) return;
      select.innerHTML = `<option value="">${I18n.t('playground.no_flow')}</option>` +
        flows.filter(f => f.is_active).map(f =>
          `<option value="${Utils.escapeAttr(String(f.id))}">${Utils.escapeHtml(f.name)}</option>`
        ).join('');
    } catch (e) {
      console.error('Blad ladowania flows:', e);
    }
  }

  // ---------------------------------------------------------------------------
  // Wysyłanie wiadomości
  // ---------------------------------------------------------------------------

  function handleSendClick() {
    if (isStreaming) {
      if (abortController) abortController.abort();
      return;
    }
    sendMessage();
  }

  async function sendMessage() {
    const textarea = document.getElementById('chat-input');
    if (!textarea) return;

    const text = textarea.value.trim();
    if (!text && attachments.length === 0) return;

    // Utwórz wiadomość użytkownika
    const userMsg = { role: 'user', timestamp: Date.now() };

    if (attachments.length > 0) {
      // Format multimodalny - Parts
      const parts = [];
      if (text) parts.push({ type: 'text', text: text });
      for (const a of attachments) {
        parts.push({ type: 'image_url', image_url: { url: a.dataUrl } });
      }
      userMsg.content = parts;
      userMsg.images = attachments.map(a => a.dataUrl);
    } else {
      userMsg.content = text;
    }

    messages.push(userMsg);
    appendMessageBubble(userMsg);

    // Wyczyść input
    textarea.value = '';
    autoResize(textarea);
    attachments = [];
    renderAttachments();

    // Parsowanie wartosci selectora modelu
    const modelValue = selectedModel || '';
    let modelName = modelValue;
    let nodeId = null;
    if (modelValue.startsWith('alias:')) {
      modelName = modelValue.substring(6);
    } else if (modelValue.startsWith('model:')) {
      modelName = modelValue.substring(6);
    } else if (modelValue.startsWith('node:')) {
      const parts = modelValue.substring(5).split(':');
      nodeId = parts[0] || null;
      modelName = parts.slice(1).join(':') || '';
    }

    // Przygotuj żądanie
    const req = {
      model: modelName,
      messages: buildApiMessages(),
      stream: true,
      temperature: temperature,
      max_tokens: maxTokens,
      top_p: topP
    };

    if (nodeId) {
      req.node_id = nodeId;
    }

    if (selectedFlow) {
      req.flow_id = selectedFlow;
    }

    await streamChat(req);
  }

  // ---------------------------------------------------------------------------
  // Budowanie tablicy wiadomości dla API
  // ---------------------------------------------------------------------------

  function buildApiMessages() {
    const apiMsgs = [];
    if (systemPrompt.trim()) {
      apiMsgs.push({ role: 'system', content: systemPrompt });
    }
    for (const msg of messages) {
      apiMsgs.push({ role: msg.role, content: msg.content });
    }
    return apiMsgs;
  }

  // ---------------------------------------------------------------------------
  // Streaming odpowiedzi
  // ---------------------------------------------------------------------------

  async function streamChat(req) {
    isStreaming = true;
    abortController = new AbortController();
    updateSendButton();

    // Dodaj pustą wiadomość asystenta
    const assistantMsg = {
      role: 'assistant',
      content: '',
      reasoning_content: '',
      timestamp: Date.now()
    };
    messages.push(assistantMsg);
    const msgEl = appendMessageBubble(assistantMsg);
    const contentEl = msgEl.querySelector('.msg-content');
    const reasoningEl = msgEl.querySelector('.msg-reasoning');
    const startTime = Date.now();
    let firstTokenTime = null;
    let tokenCount = 0;

    try {
      const response = await fetch('/api/chat/completions', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${ApiClient.getToken()}`
        },
        body: JSON.stringify(req),
        signal: abortController.signal
      });

      if (!response.ok) {
        const err = await response.json().catch(() => ({}));
        if (err.route_metadata) {
          appendRoutingErrorCard(msgEl, err);
          return;
        }
        throw new Error(err.error || `${I18n.t('playground.error_server')}: ${response.status}`);
      }

      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';
      let currentEvent = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });

        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          if (line.startsWith('event: ')) {
            currentEvent = line.slice(7).trim();
            continue;
          }
          if (!line.startsWith('data: ')) continue;
          const data = line.slice(6).trim();
          if (data === '[DONE]') { currentEvent = ''; continue; }

          // Obsluga route_info — badge z metadanymi trasy
          if (currentEvent === 'route_info') {
            try {
              const routeData = JSON.parse(data);
              appendRouteBadge(msgEl, routeData);
            } catch (_e) { /* ignoruj */ }
            currentEvent = '';
            continue;
          }
          currentEvent = '';

          try {
            const chunk = JSON.parse(data);
            const delta = chunk.choices?.[0]?.delta;
            if (delta?.content) {
              // TTFT — rejestruj moment pierwszego tokena z trescia
              if (!firstTokenTime && delta.content.length > 0) {
                firstTokenTime = Date.now();
              }
              tokenCount++;
              assistantMsg.content += delta.content;
              contentEl.innerHTML = renderMarkdown(assistantMsg.content);
            }
            if (delta?.reasoning_content) {
              assistantMsg.reasoning_content += delta.reasoning_content;
              if (reasoningEl) {
                reasoningEl.innerHTML = renderMarkdown(assistantMsg.reasoning_content);
                reasoningEl.closest('.msg-reasoning-wrapper').hidden = false;
              }
            }
            // Tokeny z usage
            if (chunk.usage) {
              assistantMsg.tokens = chunk.usage;
            }
          } catch (_e) { /* ignoruj bledy parsowania chunkow */ }
        }
        scrollToBottom();
      }

      const endTime = Date.now();
      assistantMsg.duration = ((endTime - startTime) / 1000).toFixed(1);
      // TTFT w milisekundach
      assistantMsg.ttft = firstTokenTime ? firstTokenTime - startTime : null;
      // Czas generowania (od 1-go tokena do konca) — do poprawnego tok/s
      assistantMsg.decodeTime = firstTokenTime ? (endTime - firstTokenTime) / 1000 : null;
      // Ilosc tokenow ze streamu (jesli brak usage)
      assistantMsg.streamTokenCount = tokenCount;
      updateMessageMeta(msgEl, assistantMsg);
      addCopyButtons(msgEl);
      saveConversation();

      // TTS - automatyczne odtwarzanie
      if (ttsEnabled && assistantMsg.content) {
        handleTTS(assistantMsg.content, msgEl);
      }
    } catch (e) {
      if (e.name !== 'AbortError') {
        assistantMsg.content += `\n\n**${I18n.t('common.error')}:** ${e.message}`;
        contentEl.innerHTML = renderMarkdown(assistantMsg.content);
      }
    } finally {
      isStreaming = false;
      abortController = null;
      updateSendButton();
    }
  }

  // ---------------------------------------------------------------------------
  // TTS - synteza mowy
  // ---------------------------------------------------------------------------

  async function handleTTS(text, msgEl) {
    try {
      const resp = await fetch('/api/chat/tts', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${ApiClient.getToken()}`
        },
        body: JSON.stringify({ model: 'tts-1', input: text, voice: ttsVoice })
      });

      if (!resp.ok) return;

      const arrayBuffer = await resp.arrayBuffer();
      const audioCtx = new (window.AudioContext || window.webkitAudioContext)();
      const audioBuffer = await audioCtx.decodeAudioData(arrayBuffer);

      // Odtwórz od razu
      const source = audioCtx.createBufferSource();
      source.buffer = audioBuffer;
      source.connect(audioCtx.destination);
      source.start(0);

      // Dodaj przycisk ponownego odtworzenia
      const playBtn = msgEl.querySelector('.msg-play-btn');
      if (playBtn) {
        playBtn.hidden = false;
        playBtn.onclick = () => {
          const s = audioCtx.createBufferSource();
          s.buffer = audioBuffer;
          s.connect(audioCtx.destination);
          s.start(0);
        };
      }
    } catch (e) {
      console.error('TTS error:', e);
    }
  }

  // ---------------------------------------------------------------------------
  // STT - konwersja audio do WAV (16kHz mono) po stronie klienta
  // ---------------------------------------------------------------------------

  async function convertToWav(blob) {
    const audioCtx = new (window.AudioContext || window.webkitAudioContext)({ sampleRate: 16000 });
    const arrayBuffer = await blob.arrayBuffer();
    const audioBuffer = await audioCtx.decodeAudioData(arrayBuffer);

    const offlineCtx = new OfflineAudioContext(1, audioBuffer.duration * 16000, 16000);
    const source = offlineCtx.createBufferSource();
    source.buffer = audioBuffer;
    source.connect(offlineCtx.destination);
    source.start(0);
    const rendered = await offlineCtx.startRendering();

    const pcm = rendered.getChannelData(0);
    const wavBuffer = encodeWav(pcm, 16000);

    const uint8 = new Uint8Array(wavBuffer);
    let binary = '';
    for (let i = 0; i < uint8.length; i++) {
      binary += String.fromCharCode(uint8[i]);
    }
    audioCtx.close();
    return btoa(binary);
  }

  function encodeWav(samples, sampleRate) {
    const buffer = new ArrayBuffer(44 + samples.length * 2);
    const view = new DataView(buffer);

    writeString(view, 0, 'RIFF');
    view.setUint32(4, 36 + samples.length * 2, true);
    writeString(view, 8, 'WAVE');

    writeString(view, 12, 'fmt ');
    view.setUint32(16, 16, true);
    view.setUint16(20, 1, true);
    view.setUint16(22, 1, true);
    view.setUint32(24, sampleRate, true);
    view.setUint32(28, sampleRate * 2, true);
    view.setUint16(32, 2, true);
    view.setUint16(34, 16, true);

    writeString(view, 36, 'data');
    view.setUint32(40, samples.length * 2, true);

    let offset = 44;
    for (let i = 0; i < samples.length; i++, offset += 2) {
      const s = Math.max(-1, Math.min(1, samples[i]));
      view.setInt16(offset, s < 0 ? s * 0x8000 : s * 0x7FFF, true);
    }

    return buffer;
  }

  function writeString(view, offset, string) {
    for (let i = 0; i < string.length; i++) {
      view.setUint8(offset + i, string.charCodeAt(i));
    }
  }

  // ---------------------------------------------------------------------------
  // STT - wybor i ladowanie modelu Whisper
  // ---------------------------------------------------------------------------

  async function checkSttModelStatus() {
    try {
      const resp = await fetch('/api/chat/capabilities', {
        headers: { 'Authorization': `Bearer ${ApiClient.getToken()}` }
      });
      const data = await resp.json();
      if (data.stt_local && data.stt_model) {
        sttModelLoaded = true;
        const status = document.getElementById('chat-stt-status');
        const loadText = document.getElementById('chat-stt-load-text');
        if (status) status.textContent = `✓ ${data.stt_model.name} (${data.stt_model.device || 'cpu'})`;
        if (loadText) loadText.textContent = I18n.t('playground.stt_unload', 'Unload');
      }
    } catch (e) { /* ignore */ }
  }

  async function handleSttLoadModel() {
    const btn = document.getElementById('chat-stt-load-btn');
    const status = document.getElementById('chat-stt-status');
    const loadText = document.getElementById('chat-stt-load-text');

    if (btn) btn.disabled = true;

    if (sttModelLoaded) {
      if (status) status.textContent = I18n.t('playground.stt_unloading', 'Unloading...');
      try {
        await fetch('/api/chat/stt/unload', {
          method: 'POST',
          headers: { 'Authorization': `Bearer ${ApiClient.getToken()}` }
        });
        sttModelLoaded = false;
        if (status) status.textContent = '';
        if (loadText) loadText.textContent = I18n.t('playground.stt_load', 'Load');
      } catch (e) {
        if (status) status.textContent = `✗ ${e.message}`;
      }
    } else {
      if (status) status.textContent = I18n.t('playground.stt_loading', 'Downloading & loading...');
      try {
        const resp = await fetch('/api/chat/stt/load', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${ApiClient.getToken()}`
          },
          body: JSON.stringify({})
        });
        const data = await resp.json();
        if (resp.ok) {
          sttModelLoaded = true;
          if (status) status.textContent = `✓ ${data.name || 'large-v3-turbo'} (${data.device || 'cpu'})`;
          if (loadText) loadText.textContent = I18n.t('playground.stt_unload', 'Unload');
        } else {
          if (status) status.textContent = `✗ ${data.error || 'Error'}`;
        }
      } catch (e) {
        if (status) status.textContent = `✗ ${e.message}`;
      }
    }
    if (btn) btn.disabled = false;
  }

  // ---------------------------------------------------------------------------
  // STT - rozpoznawanie mowy
  // ---------------------------------------------------------------------------

  async function handleSTT() {
    const micBtn = document.getElementById('chat-mic-btn');

    if (mediaRecorder && mediaRecorder.state === 'recording') {
      mediaRecorder.stop();
      if (micBtn) micBtn.classList.remove('recording');
      return;
    }

    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      mediaRecorder = new MediaRecorder(stream);
      audioChunks = [];

      mediaRecorder.ondataavailable = (e) => audioChunks.push(e.data);

      mediaRecorder.onstop = async () => {
        stream.getTracks().forEach(t => t.stop());
        const blob = new Blob(audioChunks, { type: mediaRecorder.mimeType });

        try {
          const wavBase64 = await convertToWav(blob);
          const resp = await fetch('/api/chat/stt', {
            method: 'POST',
            headers: {
              'Content-Type': 'application/json',
              'Authorization': `Bearer ${ApiClient.getToken()}`
            },
            body: JSON.stringify({
              audio: wavBase64,
              model: 'whisper-1',
              language: sttLanguage
            })
          });
          const data = await resp.json();
          if (data.text) {
            const textarea = document.getElementById('chat-input');
            if (textarea) {
              textarea.value += data.text;
              autoResize(textarea);
            }
          }
        } catch (e) {
          console.error('STT error:', e);
        }
      };

      mediaRecorder.start();
      if (micBtn) micBtn.classList.add('recording');
    } catch (e) {
      console.error('Blad dostepu do mikrofonu:', e);
    }
  }

  // ---------------------------------------------------------------------------
  // Załączniki - obsługa plików
  // ---------------------------------------------------------------------------

  function handleFileAttach() {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = 'image/jpeg,image/png,image/gif,image/webp';
    input.multiple = true;
    input.onchange = () => {
      for (const file of input.files) {
        const reader = new FileReader();
        reader.onload = () => {
          attachments.push({ name: file.name, type: file.type, dataUrl: reader.result });
          renderAttachments();
        };
        reader.readAsDataURL(file);
      }
    };
    input.click();
  }

  function renderAttachments() {
    const container = document.getElementById('chat-attachments');
    if (!container) return;
    container.innerHTML = attachments.map((a, i) => `
      <div class="attachment-chip">
        <img src="${a.dataUrl}" alt="${Utils.escapeHtml(a.name)}" class="attachment-thumb">
        <span>${Utils.escapeHtml(a.name)}</span>
        <button class="attachment-remove" data-idx="${i}">&times;</button>
      </div>
    `).join('');
    container.hidden = attachments.length === 0;
    container.querySelectorAll('.attachment-remove').forEach(btn => {
      btn.onclick = () => {
        attachments.splice(parseInt(btn.dataset.idx), 1);
        renderAttachments();
      };
    });
  }

  // ---------------------------------------------------------------------------
  // Konwersacje - localStorage
  // ---------------------------------------------------------------------------

  function loadConversations() {
    try {
      conversations = JSON.parse(localStorage.getItem(STORAGE_KEY) || '[]');
    } catch (_e) {
      conversations = [];
    }
    renderConversationList();
  }

  function saveConversation() {
    if (!currentConvId) return;
    const idx = conversations.findIndex(c => c.id === currentConvId);
    const conv = {
      id: currentConvId,
      title: getConversationTitle(),
      messages: messages,
      model: selectedModel,
      systemPrompt: systemPrompt,
      updatedAt: Date.now()
    };
    if (idx >= 0) {
      conversations[idx] = conv;
    } else {
      conversations.unshift(conv);
    }
    localStorage.setItem(STORAGE_KEY, JSON.stringify(conversations));
    renderConversationList();
  }

  function loadConversation(id) {
    const conv = conversations.find(c => c.id === id);
    if (!conv) return;
    currentConvId = conv.id;
    messages = conv.messages || [];
    systemPrompt = conv.systemPrompt || '';
    const sp = document.getElementById('chat-system-prompt');
    if (sp) sp.value = systemPrompt;
    renderAllMessages();
    renderConversationList();
  }

  function newConversation() {
    currentConvId = crypto.randomUUID();
    messages = [];
    renderAllMessages();
    renderConversationList();
  }

  function getConversationTitle() {
    const firstUser = messages.find(m => m.role === 'user');
    if (!firstUser) return I18n.t('playground.new_conversation_full');
    const text = typeof firstUser.content === 'string'
      ? firstUser.content
      : 'Wiadomość z obrazem';
    return text.substring(0, 50) + (text.length > 50 ? '...' : '');
  }

  function renderConversationList() {
    const container = document.getElementById('chat-sidebar-list');
    if (!container) return;

    // Grupowanie po dacie
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
    const yesterday = today - 86400000;
    const weekAgo = today - 7 * 86400000;

    const groups = { today: [], yesterday: [], week: [], older: [] };
    for (const c of conversations) {
      const t = c.updatedAt || 0;
      if (t >= today) groups.today.push(c);
      else if (t >= yesterday) groups.yesterday.push(c);
      else if (t >= weekAgo) groups.week.push(c);
      else groups.older.push(c);
    }

    let html = '';
    const labels = {
      today: I18n.t('playground.today') || 'Dzisiaj',
      yesterday: I18n.t('playground.yesterday') || 'Wczoraj',
      week: I18n.t('playground.last_week') || 'Poprzedni tydzie\u0144',
      older: I18n.t('playground.older') || 'Starsze'
    };

    for (const [key, items] of Object.entries(groups)) {
      if (items.length === 0) continue;
      html += `<div class="chat-sidebar-group-label">${labels[key]}</div>`;
      for (const c of items) {
        const active = c.id === currentConvId ? ' active' : '';
        const title = Utils.escapeHtml(c.title || I18n.t('playground.new_conversation_full'));
        html += `<button class="chat-sidebar-item${active}" data-conv-id="${c.id}" title="${title}">${title}</button>`;
      }
    }

    container.innerHTML = html;

    // Podpiecie klikniec
    container.querySelectorAll('.chat-sidebar-item').forEach(btn => {
      btn.addEventListener('click', () => {
        loadConversation(btn.dataset.convId);
        // Zamknij sidebar na mobile
        const sidebar = document.getElementById('chat-sidebar');
        if (sidebar) sidebar.classList.remove('open');
      });
    });
  }

  function renderAllMessages() {
    const container = document.getElementById('chat-messages');
    if (!container) return;
    container.innerHTML = '';
    messages.forEach(msg => appendMessageBubble(msg));
  }

  // ---------------------------------------------------------------------------
  // Bąbelki wiadomości
  // ---------------------------------------------------------------------------

  function appendMessageBubble(msg) {
    const container = document.getElementById('chat-messages');

    // Wiersz: awatar + babelka
    const row = document.createElement('div');
    row.className = `chat-msg-row chat-msg-row-${msg.role}`;

    // Awatar
    const avatarClass = msg.role === 'user' ? 'chat-avatar-user' : 'chat-avatar-assistant';
    const avatarContent = msg.role === 'user' ? 'U' : 'AI';
    const avatarHtml = `<div class="chat-avatar ${avatarClass}">${avatarContent}</div>`;

    const div = document.createElement('div');
    div.className = `chat-bubble chat-bubble-${msg.role}`;

    let html = '';

    // Blok reasoning dla asystenta
    if (msg.role === 'assistant') {
      html += `<div class="msg-reasoning-wrapper" ${msg.reasoning_content ? '' : 'hidden'}>
        <details class="msg-reasoning-details">
          <summary data-i18n="playground.reasoning">${I18n.t('playground.reasoning')}</summary>
          <div class="msg-reasoning">${msg.reasoning_content ? renderMarkdown(msg.reasoning_content) : ''}</div>
        </details>
      </div>`;
    }

    // Tresc wiadomosci
    const textContent = typeof msg.content === 'string' ? msg.content : '';
    html += `<div class="msg-content">${textContent ? renderMarkdown(textContent) : ''}</div>`;

    // Obrazy uzytkownika
    if (msg.images && msg.images.length) {
      html += `<div class="msg-images">${msg.images.map(img =>
        `<img src="${img}" class="msg-image-thumb" onclick="window.open(this.src,'_blank')">`
      ).join('')}</div>`;
    }

    // Meta asystenta - przycisk play, statystyki
    if (msg.role === 'assistant') {
      html += `<div class="msg-meta">
        <button class="msg-play-btn btn-icon" hidden title="${I18n.t('playground.play_tts')}" data-i18n-title="playground.play_tts">&#9654;</button>
        <span class="msg-stats">${msg.duration ? msg.duration + 's' : ''}</span>
      </div>`;
    }

    div.innerHTML = html;
    row.innerHTML = avatarHtml;
    row.appendChild(div);
    container.appendChild(row);

    // Dodaj przyciski Copy do blokow kodu
    div.querySelectorAll('pre').forEach(pre => {
      const btn = document.createElement('button');
      btn.className = 'code-copy-btn';
      btn.textContent = 'Copy';
      btn.addEventListener('click', () => {
        const code = pre.querySelector('code');
        const text = code ? code.textContent : pre.textContent;
        navigator.clipboard.writeText(text).then(() => {
          btn.textContent = 'Copied!';
          btn.classList.add('copied');
          setTimeout(() => { btn.textContent = 'Copy'; btn.classList.remove('copied'); }, 2000);
        });
      });
      pre.appendChild(btn);
    });

    scrollToBottom();
    return div;
  }

  // ---------------------------------------------------------------------------
  // Prosty renderer Markdown
  // ---------------------------------------------------------------------------

  function renderMarkdown(text) {
    if (!text) return '';

    // Escapuj HTML
    let html = Utils.escapeHtml(text);

    // Bloki kodu ``` ... ```
    html = html.replace(/```(\w*)\n([\s\S]*?)```/g, (_, lang, code) => {
      const cls = lang ? ` class="language-${lang}"` : '';
      return `<pre><code${cls}>${code}</code></pre>`;
    });

    // Inline code
    html = html.replace(/`([^`]+)`/g, '<code>$1</code>');

    // Pogrubienie
    html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');

    // Kursywa
    html = html.replace(/\*(.+?)\*/g, '<em>$1</em>');

    // Lista nieuporządkowana
    html = html.replace(/^[-*] (.+)$/gm, '<li>$1</li>');
    html = html.replace(/((?:<li>.*<\/li>\n?)+)/g, '<ul>$1</ul>');

    // Lista uporządkowana
    html = html.replace(/^\d+\. (.+)$/gm, '<li>$1</li>');
    html = html.replace(/((?:<li>.*<\/li>\n?)+)/g, (match) => {
      // Unikaj podwójnego zawijania
      if (match.startsWith('<ul>')) return match;
      return `<ol>${match}</ol>`;
    });

    // Akapity (podwójny enter)
    html = html.replace(/\n\n/g, '</p><p>');
    html = `<p>${html}</p>`;

    // Pojedyncze entery w ramach akapitu
    html = html.replace(/\n/g, '<br>');

    // Sprzątanie pustych paragrafów
    html = html.replace(/<p><\/p>/g, '');
    html = html.replace(/<p>(<pre>)/g, '$1');
    html = html.replace(/(<\/pre>)<\/p>/g, '$1');
    html = html.replace(/<p>(<ul>)/g, '$1');
    html = html.replace(/(<\/ul>)<\/p>/g, '$1');
    html = html.replace(/<p>(<ol>)/g, '$1');
    html = html.replace(/(<\/ol>)<\/p>/g, '$1');

    return html;
  }

  // ---------------------------------------------------------------------------
  // Pomocnicze
  // ---------------------------------------------------------------------------

  function addCopyButtons(el) {
    el.querySelectorAll('pre').forEach(pre => {
      if (pre.querySelector('.code-copy-btn')) return;
      const btn = document.createElement('button');
      btn.className = 'code-copy-btn';
      btn.textContent = 'Copy';
      btn.addEventListener('click', () => {
        const code = pre.querySelector('code');
        const text = code ? code.textContent : pre.textContent;
        navigator.clipboard.writeText(text).then(() => {
          btn.textContent = 'Copied!';
          btn.classList.add('copied');
          setTimeout(() => { btn.textContent = 'Copy'; btn.classList.remove('copied'); }, 2000);
        });
      });
      pre.appendChild(btn);
    });
  }

  function scrollToBottom() {
    const container = document.getElementById('chat-messages');
    if (container) container.scrollTop = container.scrollHeight;
  }

  function autoResize(textarea) {
    textarea.style.height = 'auto';
    textarea.style.height = Math.min(textarea.scrollHeight, 200) + 'px';
  }

  function updateSendButton() {
    const btn = document.getElementById('chat-send-btn');
    if (!btn) return;
    if (isStreaming) {
      btn.innerHTML = '<svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="6" width="12" height="12" rx="2"></rect></svg>';
      btn.title = I18n.t('playground.stop');
      btn.setAttribute('data-i18n-title', 'playground.stop');
    } else {
      btn.innerHTML = '<svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor"><path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"></path></svg>';
      btn.title = I18n.t('playground.send');
      btn.setAttribute('data-i18n-title', 'playground.send');
    }
  }

  function updateMessageMeta(el, msg) {
    const stats = el.querySelector('.msg-stats');
    if (!stats) return;
    let parts = [];

    // Calkowity czas
    if (msg.duration) parts.push(msg.duration + 's');

    // TTFT (Time To First Token)
    if (msg.ttft != null) {
      if (msg.ttft < 1000) {
        parts.push('TTFT ' + msg.ttft + 'ms');
      } else {
        parts.push('TTFT ' + (msg.ttft / 1000).toFixed(1) + 's');
      }
    }

    // Ilosc tokenow
    const completionTokens = (msg.tokens && msg.tokens.completion_tokens)
      ? msg.tokens.completion_tokens
      : msg.streamTokenCount || 0;
    if (completionTokens > 0) {
      parts.push(completionTokens + ' tok');
    }

    // tok/s — liczone od momentu 1-go tokena (decodeTime), nie od startu requestu
    if (completionTokens > 0 && msg.decodeTime && msg.decodeTime > 0) {
      const tps = (completionTokens / msg.decodeTime).toFixed(1);
      parts.push(tps + ' tok/s');
    } else if (completionTokens > 0 && msg.duration) {
      // Fallback — caly czas (dla non-streaming)
      const dur = parseFloat(msg.duration);
      if (dur > 0) {
        const tps = (completionTokens / dur).toFixed(1);
        parts.push(tps + ' tok/s');
      }
    } else if (msg.content && msg.duration) {
      // Estymacja jesli brak tokenow
      const dur = parseFloat(msg.duration);
      if (dur > 0) {
        const estimatedTokens = Math.ceil(msg.content.length / 4);
        const tps = (estimatedTokens / dur).toFixed(1);
        parts.push('~' + tps + ' tok/s');
      }
    }

    stats.textContent = parts.join(' | ');
  }

  function toggleParams() {
    const panel = document.getElementById('chat-params-panel');
    if (!panel) return;
    paramsVisible = !paramsVisible;
    panel.hidden = !paramsVisible;
  }

  function bindParamListeners() {
    // System prompt
    const sp = document.getElementById('chat-system-prompt');
    if (sp) {
      sp.addEventListener('input', () => { systemPrompt = sp.value; });
    }

    // Temperature
    const tempSlider = document.getElementById('chat-temperature');
    const tempValue = document.getElementById('chat-temp-value');
    if (tempSlider) {
      tempSlider.addEventListener('input', () => {
        temperature = parseFloat(tempSlider.value);
        if (tempValue) tempValue.textContent = temperature;
      });
    }

    // Max tokens
    const maxTok = document.getElementById('chat-max-tokens');
    if (maxTok) {
      maxTok.addEventListener('change', () => {
        maxTokens = parseInt(maxTok.value) || 4096;
      });
    }

    // Top-p
    const topPSlider = document.getElementById('chat-top-p');
    const topPValue = document.getElementById('chat-topp-value');
    if (topPSlider) {
      topPSlider.addEventListener('input', () => {
        topP = parseFloat(topPSlider.value);
        if (topPValue) topPValue.textContent = topP;
      });
    }

    // Głos TTS
    const voiceSelect = document.getElementById('chat-voice');
    if (voiceSelect) {
      voiceSelect.addEventListener('change', () => { ttsVoice = voiceSelect.value; });
    }

    // Język STT
    const langSelect = document.getElementById('chat-language');
    if (langSelect) {
      langSelect.addEventListener('change', () => { sttLanguage = langSelect.value; });
    }
  }

  // ---------------------------------------------------------------------------
  // Route badge — widoczny tylko gdy serwer wysle event route_info
  // ---------------------------------------------------------------------------

  function appendRouteBadge(bubbleEl, routeData) {
    if (!bubbleEl || !routeData) return;
    const nodeRaw = routeData.served_by_node || '-';
    const nodeShort = nodeRaw.length > 12 ? nodeRaw.substring(0, 12) + '\u2026' : nodeRaw;
    const strategy = routeData.strategy_used || '-';
    const stratLabel = I18n.t('models.strategy_' + strategy) || strategy;
    const fallbackCount = (routeData.fallbacks_tried || []).length;

    const badge = document.createElement('div');
    badge.className = 'route-badge';
    badge.setAttribute('role', 'status');
    badge.setAttribute('tabindex', '0');
    badge.style.cssText = 'background:var(--color-info-light);color:var(--color-info);border-radius:var(--radius-sm);padding:2px 8px;font-size:var(--font-size-xs);cursor:pointer;min-height:44px;display:inline-flex;align-items:center;margin-top:4px;';
    if (nodeRaw.length > 12) badge.title = nodeRaw;

    const summaryText = `${nodeShort} | ${stratLabel} | ${fallbackCount} fallbacks`;
    badge.textContent = summaryText;

    // Szczegoly — rozwijane po kliknieciu
    const details = document.createElement('div');
    details.hidden = true;
    details.style.cssText = 'font-family:monospace;font-size:var(--font-size-xs);margin-top:4px;white-space:pre-wrap;';

    const detailLines = [
      `served_by_node: ${routeData.served_by_node || '-'}`,
      `backend_type: ${routeData.backend_type || '-'}`,
      `strategy_used: ${strategy}`,
      `fallbacks_tried: ${(routeData.fallbacks_tried || []).join(', ') || '-'}`,
      `hop_count: ${routeData.hop_count ?? '-'}`,
      `latency_ms: ${routeData.latency_ms ?? '-'}`
    ];
    details.textContent = detailLines.join('\n');

    const toggle = () => { details.hidden = !details.hidden; };
    badge.addEventListener('click', toggle);
    badge.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); }
    });

    const wrapper = document.createElement('div');
    wrapper.appendChild(badge);
    wrapper.appendChild(details);
    bubbleEl.appendChild(wrapper);
  }

  // ---------------------------------------------------------------------------
  // Routing error card — blad routingu z metadanymi
  // ---------------------------------------------------------------------------

  function appendRoutingErrorCard(bubbleEl, errData) {
    if (!bubbleEl) return;
    const meta = errData.route_metadata || {};
    const modelName = errData.model || meta.model || '-';
    const strategy = meta.strategy_used || '-';
    const stratLabel = I18n.t('models.strategy_' + strategy) || strategy;
    const targets = meta.targets_tried || [];

    const card = document.createElement('div');
    card.className = 'route-error-card';
    card.style.cssText = 'background:var(--color-error-light);border:1px solid var(--color-error);border-radius:var(--radius-md);padding:12px 16px;margin-top:4px;width:100%;';

    let html = `<div style="font-weight:bold;margin-bottom:8px;">\u26A0 ${Utils.escapeHtml(I18n.t('chat.routing_failed').replace('{model}', modelName))}</div>`;

    if (targets.length > 0) {
      html += `<div style="margin-bottom:8px;">${Utils.escapeHtml(I18n.t('chat.targets_tried'))}</div>`;
      html += '<div style="margin-left:8px;">';
      for (const t of targets) {
        const name = typeof t === 'string' ? t : (t.name || t.target || '-');
        const reason = typeof t === 'object' ? (t.reason || '') : '';
        html += `<div>\u2717 ${Utils.escapeHtml(name)}${reason ? ' (' + Utils.escapeHtml(reason) + ')' : ''}</div>`;
      }
      html += '</div>';
    }

    html += `<div style="margin-top:8px;">${I18n.t('common.strategy')}: ${Utils.escapeHtml(stratLabel)}</div>`;
    html += `<div style="margin-top:8px;"><a href="#" class="route-error-configure" style="color:var(--color-primary);text-decoration:underline;">${Utils.escapeHtml(I18n.t('chat.configure_alias'))} \u2192</a></div>`;

    card.innerHTML = html;

    // Nawigacja do modeli
    const link = card.querySelector('.route-error-configure');
    if (link) {
      link.addEventListener('click', (e) => {
        e.preventDefault();
        ViewRouter.navigate('models');
      });
    }

    bubbleEl.appendChild(card);
  }

  // ---------------------------------------------------------------------------
  // Publiczny interfejs
  // ---------------------------------------------------------------------------

  return { render, mount, unmount };

})();
