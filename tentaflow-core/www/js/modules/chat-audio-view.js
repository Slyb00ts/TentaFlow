// =============================================================================
// File: modules/chat-audio-view.js — audio stage of the Chat screen: the face,
// the state pill, the wave, the control dock and the live transcript. Pure view:
// the AudioPipeline and the stream subscriptions stay in chat.js, which feeds
// this module state changes and playback events.
// =============================================================================

import { escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { extractPlainText } from '/js/lib/md-lite.js';
import { splitSentences } from '/js/modules/chat-audio.js';
import { splitWords, wordOffsets, sentenceForChunk } from '/js/lib/chat-transcript.js';

const WAVE_BARS = 34;
// Older turns are not worth re-rendering on every delta.
const TRANSCRIPT_WINDOW = 30;
const MIC_PLACEHOLDER = '🎤';

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function clock(ts) {
  if (!ts) return '';
  const d = new Date(ts);
  const pad = (n) => String(n).padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function waveMarkup() {
  let html = '';
  for (let i = 0; i < WAVE_BARS; i += 1) {
    const delay = (-Math.random() * 0.9).toFixed(2);
    const dur = (0.7 + Math.random() * 0.5).toFixed(2);
    html += `<i class="wave-bar" style="animation-delay:${delay}s;animation-duration:${dur}s"></i>`;
  }
  return html;
}

function markup() {
  const t = (k) => escapeHtml(I18n.t(`chat.${k}`));
  return `
    <div class="audio-body">
      <div class="face-stage" id="chat-face-stage" data-state="idle">
        <div class="stage-top"><div class="audio-status" id="audio-status" role="status"><span class="dot"></span><span id="audio-status-label">${t('audio_state_idle')}</span></div></div>
        <div class="stage-bottom">
          <div class="wave" aria-hidden="true">${waveMarkup()}</div>
          <div class="audio-controls">
            <tf-button class="dock-btn" variant="ghost" icon="volume" id="audio-volume" disabled aria-label="${t('audio_speaker')}" title="${t('audio_speaker')}"></tf-button>
            <tf-button class="dock-btn primary" variant="primary" icon="mic" id="audio-mic" aria-label="${t('audio_start_mic')}" title="${t('audio_start_mic')}"></tf-button>
            <tf-button class="dock-btn" variant="ghost" icon="pause" id="audio-pause" disabled aria-label="${t('audio_pause')}" title="${t('audio_pause')}"></tf-button>
            <tf-button class="dock-btn end" variant="danger-solid" icon="x" id="audio-exit" aria-label="${t('audio_exit')}" title="${t('audio_exit')}"></tf-button>
          </div>
        </div>
      </div>
      <aside class="transcript" aria-label="${t('transcript_title')}">
        <div class="tr-head"><h3>${sprite('message')}${t('transcript_title')}</h3><tf-chip id="audio-chip" variant="outline" status="neutral" dot>${t('audio_chip_idle')}</tf-chip></div>
        <div class="tr-list" id="audio-transcript" aria-live="polite"></div>
      </aside>
    </div>`;
}

/**
 * Mounts the audio stage into `host` and returns its controller. `handlers`
 * carries the four dock actions; everything else is driven through the
 * returned methods.
 */
export function createAudioView(host, handlers) {
  host.innerHTML = markup();
  const stage = host.querySelector('#chat-face-stage');
  const list = host.querySelector('#audio-transcript');
  const label = host.querySelector('#audio-status-label');
  const chip = host.querySelector('#audio-chip');
  const volume = host.querySelector('#audio-volume');
  const mic = host.querySelector('#audio-mic');
  const pause = host.querySelector('#audio-pause');

  const state = {
    conv: null,
    userSpeaking: false,
    // The sentence and word now being spoken, null outside of speech.
    speech: null,
    timers: new Set(),
  };

  volume.addEventListener('click', () => handlers.onVolume());
  mic.addEventListener('click', () => handlers.onMic());
  pause.addEventListener('click', () => handlers.onPause());
  host.querySelector('#audio-exit').addEventListener('click', () => handlers.onExit());

  function later(fn, ms) {
    const id = setTimeout(() => {
      state.timers.delete(id);
      fn();
    }, Math.max(0, ms));
    state.timers.add(id);
  }

  function clearTimers() {
    for (const id of state.timers) clearTimeout(id);
    state.timers.clear();
  }

  function paintSpeech() {
    const sp = state.speech;
    for (const sent of list.querySelectorAll('.sent')) {
      const s = Number(sent.dataset.s);
      sent.classList.toggle('cur', !!sp && s === sp.sent);
      for (const w of sent.querySelectorAll('.w')) {
        const i = Number(w.dataset.w);
        const said = !!sp && (s < sp.sent || (s === sp.sent && i < sp.word));
        w.classList.toggle('said', said);
        w.classList.toggle('cur', !!sp && s === sp.sent && i === sp.word);
      }
    }
  }

  function whoMarkup(role, ts) {
    const name = role === 'user' ? I18n.t('chat.you') : I18n.t('chat.assistant');
    return `<div class="who">${escapeHtml(name)}<span class="t">${escapeHtml(clock(ts))}</span></div>`;
  }

  function assistantItem(msg, isLastAssistant) {
    const plain = extractPlainText(msg.text || '');
    const speaking = state.speech && state.speech.msgId === msg.id;
    if (!plain) {
      if (!msg.streaming) return '';
      return `<div class="tr-item bot now">${whoMarkup('assistant', msg.ts)}<div class="think-note"><span class="chat-typing"><span></span><span></span><span></span></span>${escapeHtml(I18n.t('chat.transcript_thinking'))}</div></div>`;
    }
    const now = speaking || (isLastAssistant && msg.streaming);
    let body;
    if (speaking) {
      body = splitSentences(plain).map((sentence, s) => {
        const words = splitWords(sentence).map((w, i) => `<span class="w" data-w="${i}">${escapeHtml(w)}</span>`).join(' ');
        return `<span class="sent" data-s="${s}">${words}</span>`;
      }).join(' ');
    } else {
      body = escapeHtml(plain);
    }
    return `<div class="tr-item bot${now ? ' now' : ''}" data-msg="${msg.id}">${whoMarkup('assistant', msg.ts)}<div class="text">${body}</div></div>`;
  }

  function userItem(msg) {
    const pending = msg.text === MIC_PLACEHOLDER;
    const text = pending
      ? `<div class="text partial">${escapeHtml(I18n.t('chat.transcript_recognizing'))}</div>`
      : `<div class="text">${escapeHtml(msg.text || '')}</div>`;
    return `<div class="tr-item user${pending ? ' now' : ''}">${whoMarkup('user', msg.ts)}${text}</div>`;
  }

  const api = {
    /** The element tf-face is embedded into. */
    faceHost: stage,

    /** `name` is listen | think | speak | idle; `text` overrides the pill label. */
    setState(name, text) {
      stage.dataset.state = name;
      label.textContent = text || I18n.t(`chat.audio_state_${name}`);
    },

    /** Dock availability: before the first mic click only mic and exit work. */
    setDock({ started, muted, speakerMuted }) {
      volume.toggleAttribute('disabled', !started);
      pause.toggleAttribute('disabled', !started);
      mic.classList.toggle('is-muted', !!muted && started);
      volume.classList.toggle('is-muted', !!speakerMuted);
      const micLabel = !started
        ? I18n.t('chat.audio_start_mic')
        : (muted ? I18n.t('chat.audio_unmute') : I18n.t('chat.audio_mute'));
      mic.setAttribute('aria-label', micLabel);
      mic.setAttribute('title', micLabel);
    },

    /** live | idle */
    setChip(kind) {
      const map = { live: ['ok', 'audio_chip_live'], idle: ['neutral', 'audio_chip_idle'] };
      const [status, key] = map[kind];
      chip.setAttribute('status', status);
      chip.setAttribute('label', I18n.t(`chat.${key}`));
    },

    setUserSpeaking(on) {
      if (state.userSpeaking === on) return;
      state.userSpeaking = on;
      api.render();
    },

    /** Rebuilds the transcript from the conversation's messages. */
    render(conv = state.conv) {
      state.conv = conv;
      if (!conv) return;
      const nearBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 48;
      const msgs = conv.messages
        .filter((m) => m.role === 'user' || m.role === 'assistant')
        .slice(-TRANSCRIPT_WINDOW);
      let lastAssistant = -1;
      msgs.forEach((m, i) => { if (m.role === 'assistant') lastAssistant = i; });
      let html = msgs.map((m, i) => (m.role === 'user' ? userItem(m) : assistantItem(m, i === lastAssistant))).join('');
      const tailIsPending = msgs.length > 0 && msgs[msgs.length - 1].text === MIC_PLACEHOLDER;
      if (state.userSpeaking && !tailIsPending) {
        html += `<div class="tr-item user now">${whoMarkup('user', Date.now())}<div class="text partial">${escapeHtml(I18n.t('chat.transcript_speaking'))}</div></div>`;
      }
      list.innerHTML = html;
      paintSpeech();
      if (nearBottom) list.scrollTop = list.scrollHeight;
    },

    /**
     * One scheduled TTS chunk: after `startsInMs` its sentence becomes current
     * and its words light up across the chunk's real duration.
     */
    playChunk({ index, startsInMs, durationMs }, msg) {
      if (!msg) return;
      const sentences = splitSentences(extractPlainText(msg.text || ''));
      const k = sentenceForChunk(sentences, index);
      if (k < 0) return;
      const offsets = wordOffsets(splitWords(sentences[k]), durationMs);
      later(() => {
        state.speech = { msgId: msg.id, sent: k, word: 0 };
        api.render();
        offsets.forEach((at, i) => {
          if (i === 0) return;
          later(() => {
            if (!state.speech || state.speech.msgId !== msg.id || state.speech.sent !== k) return;
            state.speech.word = i;
            paintSpeech();
          }, at);
        });
      }, startsInMs);
    },

    /** Speech ended or was cut: drop the highlight and any pending word ticks. */
    stopSpeech() {
      clearTimers();
      if (!state.speech) return;
      state.speech = null;
      api.render();
    },

    destroy() {
      clearTimers();
      host.innerHTML = '';
    },
  };
  return api;
}
