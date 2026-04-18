// =============================================================================
// Plik: modules/chat/ChatBinary.js
// Opis: Chat ekran (R-STREAM archetyp) zmigrowany na binary protocol.
//       Uzywa subskrypcji streama: chunki dolaczane na biezaco do wyjscia,
//       end z usage stats konczy generowanie.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const ChatBinary = (() => {
  'use strict';
  let currentUnsubscribe = null;

  async function sendMessage(modelId, userMessage) {
    if (currentUnsubscribe) {
      currentUnsubscribe();
      currentUnsubscribe = null;
    }
    const outputEl = document.getElementById('chat-output');
    if (!outputEl) return;
    outputEl.textContent = '';

    let totalChars = 0;
    currentUnsubscribe = await ApiBinary.subscribe(
      'chatStreamRequest',
      { modelId, userMessage },
      {
        onChunk: (body) => {
          if (body.variant === 'ChatStreamChunk') {
            outputEl.textContent += body.delta;
            totalChars += body.delta.length;
          }
        },
        onEnd: (body) => {
          if (body?.variant === 'ChatStreamEnd') {
            console.log(
              `[chat-binary] tokens=${body.promptTokens}/${body.completionTokens}, chars=${totalChars}`
            );
          }
          currentUnsubscribe = null;
        },
        onError: (err) => {
          App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
          currentUnsubscribe = null;
        },
      }
    );
  }

  return {
    mount: () => {
      document.getElementById('chat-send')?.addEventListener('click', () => {
        const modelId = document.getElementById('chat-model')?.value || 'default';
        const userMessage = document.getElementById('chat-input')?.value;
        if (userMessage) sendMessage(modelId, userMessage);
      });
    },
    unmount: () => {
      if (currentUnsubscribe) currentUnsubscribe();
      currentUnsubscribe = null;
    },
    sendMessage,
  };
})();

export default ChatBinary;
