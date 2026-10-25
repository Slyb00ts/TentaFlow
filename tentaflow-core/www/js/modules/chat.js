// =============================================================================
// File: modules/chat.js — User-facing Chat app.
// Layout (mockups/chat-20261024): [conversation list | top bar of three equal
// cells (Decode, TTFT, Chat|Audio) / body / composer]. The body is a
// virtualized message list in text mode and the face stage (chat-audio-view.js)
// in audio mode; both belong to the same conversation, so switching keeps it.
// Virtualization: VirtualList mounted directly on .chat-body. The centred
// 736px column is achieved through padding-inline on .chat-body, so the vlist
// host stays full width and the scrollbar sits at the edge of the screen.
// The decode speed and TTFT in the bar come only from the stream of the
// active conversation (lib/chat-metrics.js), never from node-wide counters.
// Conversations: persisted locally (localStorage) — server history API is
// a future addition.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { measureItemHeight, getDefaultFont, getDefaultLineHeight } from '/js/lib/text-measure.js';
import { createVirtualList } from '/js/lib/virtual-list.js';
import { renderMarkdown, extractPlainText } from '/js/lib/md-lite.js';
import { DecodeMeter, SessionMetrics, sparkMarkup, fmtRate } from '/js/lib/chat-metrics.js';
import { TfWindow } from '/js/components/tf-window.js';
import FaceBackground from '/js/modules/faceBackground.js';
import { AudioPipeline } from '/js/modules/chat-audio.js';
import { createAudioView } from '/js/modules/chat-audio-view.js';
import '/js/components/tf-agent-activity.js';
import { attachAgentActivity } from '/js/lib/agent-activity-bridge.js';

const STORAGE_KEY = 'tentaflow_chat_conversations_v1';
// The seeded "Default Chat" flow (db/seed.rs) — the same id on every node.
// Chat has no flow picker: every turn, typed or spoken, runs on this one.
const DEFAULT_CHAT_FLOW_ID = '00000000-0000-4000-8000-000000000010';
const MAX_INPUT_CHARS = 4096;
const MIC_PLACEHOLDER = '🎤';
const STREAM_SAVE_MS = 400;
const SAMPLE_INTERVAL_MS = 1000;
const WEEK_MS = 7 * 86_400_000;

// Attachments are inlined into the prompt, so they are text and they are small.
const ATTACH_MAX_FILES = 5;
const ATTACH_MAX_BYTES = 256 * 1024;
const ATTACH_ACCEPT = 'text/*,.md,.json,.csv,.log,.yaml,.yml,.toml,.xml,.html,.css,.js,.ts,.py,.rs,.go,.java,.c,.cpp,.h,.sh,.sql';

// Reading column: 28px avatar + 12px gap; the user bubble is capped like the CSS.
const AVATAR_AND_GAP_PX = 28 + 12;
const USER_BUBBLE_MAX = 640;
const USER_BUBBLE_PADDING_PX = 28;
const FENCE_HEADER_PX = 30;
const THINK_COLLAPSED_PX = 40;

let conversations = [];
let activeConvId = null;
let vlist = null;
let listWidth = 736;
let nextMsgId = 1;
let searchFilter = '';

// Teardown for the per-session agent-activity run-events subscription. Re-bound
// on every conversation switch (each conversation is its own session scope).
let agentActivityTeardown = null;
// The Default Chat flow row, resolved once at mount(). Models (LLM/STT/TTS)
// are picked INSIDE that flow in the Flow Builder, so the browser sends no
// model id at all. null when the flow is missing.
let defaultChatFlow = null;

// One generation at a time: { conv, msg, meter, sampler, unsub, cancelled, kind }.
let turn = null;
// Decode/TTFT series per conversation, seeded from the answers already stored.
const metricsByConv = new Map();
// Files picked for the next message.
let pendingFiles = [];
let userInitials = '?';
// Conversation the open ⋮ menu belongs to.
let menuConvId = null;

// Server link as the UI sees it; the platform overlay owns the reconnect UI.
let conn = { lost: false };
let offLifecycle = null;

let audioView = null;
let faceHandle = null;
let audioPipeline = null;
let audioUi = { state: 'idle', text: '' };
let escKeyHandler = null;
let pageHideHandler = null;
let spaceKeydownHandler = null;
let spaceKeyupHandler = null;
let spaceHeld = false;
let bodyResizeObserver = null;

const t = (key, vars) => I18n.t(`chat.${key}`, vars);

// ---- Persistence ---------------------------------------------------------

function loadConversations() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    // A page that was closed or reloaded mid-answer saved it while streaming;
    // no stream exists any more, so it becomes an interrupted answer.
    for (const conv of parsed) {
      for (const msg of conv.messages || []) {
        if (!msg.streaming) continue;
        msg.streaming = false;
        msg.status = '';
        msg.failed = { kind: 'interrupted' };
      }
    }
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

function newConversation() {
  const id = `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
  return {
    id,
    // Empty until the first message names it or the user renames it; the list
    // shows the localized "new conversation" label meanwhile.
    title: '',
    renamed: false,
    createdAt: Date.now(),
    updatedAt: Date.now(),
    messages: [],
    mode: 'text',
    // `language` is the per-conversation transcription language, overwritten
    // from I18n.getLanguage() when the pipeline starts.
    audioConfig: { language: 'pl' },
  };
}

// Conversations stored by older builds: no mode, no audio config, and a
// localized placeholder saved as the title of an empty conversation.
function migrateConversations(list) {
  for (const c of list) {
    if (typeof c.mode !== 'string') c.mode = 'text';
    if (!c.audioConfig || typeof c.audioConfig !== 'object') c.audioConfig = { language: 'pl' };
    if (typeof c.renamed !== 'boolean') c.renamed = false;
    if (!c.renamed && c.messages.length === 0) c.title = '';
    for (const m of c.messages) delete m.modelLabel;
  }
}

function activeConv() {
  return conversations.find((c) => c.id === activeConvId) || null;
}

function convTitle(conv) {
  return conv.title || t('new_conversation');
}

function isVoiceConv(conv) {
  return conv.mode === 'audio' || conv.messages.some((m) => m.via === 'voice');
}

// ---- Small helpers -------------------------------------------------------

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function clock(ts) {
  if (!ts) return '';
  const d = new Date(ts);
  const pad = (n) => String(n).padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

// Translation strings mark emphasis with <b>; everything else stays inert text.
function richText(text) {
  return escapeHtml(text).replaceAll('&lt;b&gt;', '<b>').replaceAll('&lt;/b&gt;', '</b>');
}

function lang() {
  return (I18n.getLanguage && I18n.getLanguage()) || 'en';
}

// One line, whatever the message looked like.
function titleFrom(text) {
  const line = text.replace(/\s+/g, ' ').trim();
  return line.slice(0, 40) + (line.length > 40 ? '…' : '');
}

function startOfToday() {
  const now = new Date();
  return new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
}

// ---- Conversation list ---------------------------------------------------

function groupByDay(items) {
  const today0 = startOfToday();
  const yesterday0 = today0 - 86_400_000;
  const buckets = [
    { label: t('day_now'), items: [] },
    { label: t('day_today'), items: [] },
    { label: t('day_yesterday'), items: [] },
    { label: t('day_week'), items: [] },
    { label: t('day_earlier'), items: [] },
  ];
  for (const c of items) {
    if (c.messages.length === 0) buckets[0].items.push(c);
    else if (c.updatedAt >= today0) buckets[1].items.push(c);
    else if (c.updatedAt >= yesterday0) buckets[2].items.push(c);
    else if (c.updatedAt >= today0 - WEEK_MS) buckets[3].items.push(c);
    else buckets[4].items.push(c);
  }
  return buckets.filter((b) => b.items.length > 0);
}

// Time of day for today and yesterday, weekday within the last week, else date.
function convWhen(conv) {
  const today0 = startOfToday();
  const ts = conv.updatedAt;
  if (ts >= today0 - 86_400_000) return clock(ts);
  const d = new Date(ts);
  if (ts >= today0 - WEEK_MS) return new Intl.DateTimeFormat(lang(), { weekday: 'short' }).format(d);
  const pad = (n) => String(n).padStart(2, '0');
  return `${pad(d.getDate())}.${pad(d.getMonth() + 1)}`;
}

function convMeta(conv) {
  const when = convWhen(conv);
  if (conv.messages.length === 0) return t('conv_empty');
  return isVoiceConv(conv) ? t('voice_meta', { time: when }) : when;
}

function convMatches(conv, needle) {
  if (convTitle(conv).toLowerCase().includes(needle)) return true;
  return conv.messages.some((m) => extractPlainText(m.text || '').toLowerCase().includes(needle));
}

function renderConvItem(conv) {
  const isActive = conv.id === activeConvId;
  const voice = isVoiceConv(conv);
  const live = isActive && conv.mode === 'audio';
  return `
    <div class="conv-item${isActive ? ' active' : ''}" data-conv-id="${escapeHtml(conv.id)}" role="button" tabindex="0">
      <div class="conv-item__title">${voice ? sprite('mic') : (conv.messages.length === 0 ? sprite('message') : '')}<span>${escapeHtml(convTitle(conv))}</span>${live ? '<span class="conv-live" aria-hidden="true"></span>' : ''}</div>
      <div class="conv-item__meta">${escapeHtml(convMeta(conv))}</div>
      <tf-button class="conv-more" variant="ghost" size="sm" icon="more-v" data-conv-more="${escapeHtml(conv.id)}"
        aria-label="${escapeHtml(t('conv_more'))}" aria-haspopup="menu"></tf-button>
    </div>`;
}

function renderConvList() {
  const host = byId('chat-conv-list');
  if (!host) return;
  const needle = searchFilter.trim().toLowerCase();
  const filtered = (needle ? conversations.filter((c) => convMatches(c, needle)) : conversations.slice())
    .sort((a, b) => b.updatedAt - a.updatedAt);
  const foot = byId('chat-conv-foot');
  if (foot) foot.textContent = t('conv_footer', { count: conversations.length });
  if (filtered.length === 0) {
    host.innerHTML = `<div class="conv-empty">${escapeHtml(t('no_conversations'))}</div>`;
    return;
  }
  host.innerHTML = groupByDay(filtered)
    .map((g) => `<div class="conv-grp">${escapeHtml(g.label)}</div>${g.items.map(renderConvItem).join('')}`)
    .join('');
}

function closeDrawer() {
  byId('chat-shell')?.classList.remove('conv-open');
}

// ---- Message rendering ---------------------------------------------------

// Persisted <think> open state per (msgId, blockIdx): the virtualizer re-renders
// bubbles on scroll, and without this map a `<details>` would lose `open`.
const thinkOpenState = new Map();

function getThinkOpenState(key) {
  if (!key) return undefined;
  return thinkOpenState.has(key) ? thinkOpenState.get(key) : undefined;
}

function whoLine(msg) {
  const name = msg.role === 'user' ? t('you') : t('assistant');
  return `${escapeHtml(name)} · ${escapeHtml(clock(msg.ts))}`;
}

function attachmentChips(msg) {
  if (!msg.attachments?.length) return '';
  const chips = msg.attachments.map((a) => `<tf-chip icon="file" mono>${escapeHtml(a.name)}</tf-chip>`).join('');
  return `<div class="chat-msg__files">${chips}</div>`;
}

function metaChips(msg) {
  const p = msg.perf || {};
  const chips = [];
  if (p.decodeTps > 0) chips.push(`<tf-chip tag variant="outline" mono status="accent" icon="zap">${escapeHtml(fmtRate(p.decodeTps, lang()))} ${escapeHtml(t('perf_decode'))}</tf-chip>`);
  if (p.ttftMs > 0) chips.push(`<tf-chip tag variant="outline" mono status="neutral">${escapeHtml(t('perf_ttft'))} ${Math.round(p.ttftMs)} ms</tf-chip>`);
  if (p.completionTokens > 0) chips.push(`<tf-chip tag variant="outline" mono status="neutral">${Math.round(p.completionTokens)} ${escapeHtml(t('perf_tokens'))}</tf-chip>`);
  if (msg.stopped) chips.push(`<tf-chip tag variant="outline" mono status="warn">${escapeHtml(t('stopped'))}</tf-chip>`);
  return chips.join('');
}

// What each failure says; a fragment is only "kept" when there is one.
const FAILURE_TEXT = {
  interrupted: { kept: 'err_interrupted', none: 'err_interrupted_empty' },
  fault: { kept: 'err_generic', none: 'err_generic_empty' },
  unavailable: { kept: 'unavailable', none: 'unavailable' },
  empty: { kept: 'err_generic', none: 'err_empty' },
};

function failureBox(msg) {
  const chars = (msg.text || '').length;
  const title = chars > 0 ? t('err_title', { count: chars }) : t('err_title_empty');
  const detail = t(FAILURE_TEXT[msg.failed.kind][chars > 0 ? 'kept' : 'none']);
  return `
    <div class="chat-msg__error">
      <tf-alert tone="danger" icon="alert" inline-actions title="${escapeHtml(title)}" message="${escapeHtml(detail)}">
        <div slot="actions"><tf-button variant="secondary" size="sm" icon="play" data-act="resume">${escapeHtml(t('err_resume'))}</tf-button></div>
      </tf-alert>
    </div>`;
}

// The entry animation belongs to the first insertion only: updateTail
// rewrites the tail's markup on every token, and a CSS animation restarts
// with each new element.
const enteredMessages = new WeakSet();
function enterClass(msg) {
  if (enteredMessages.has(msg)) return '';
  enteredMessages.add(msg);
  return ' chat-msg--enter';
}

function renderMessage(index, msg) {
  const isUser = msg.role === 'user';
  const streaming = msg.streaming === true;
  if (isUser) {
    return `
      <div class="chat-msg user${enterClass(msg)}" data-msg-id="${msg.id}">
        <div class="chat-msg__av" aria-hidden="true">${escapeHtml(userInitials)}</div>
        <div class="chat-msg__body">
          <div class="chat-msg__who">${whoLine(msg)}</div>
          ${attachmentChips(msg)}
          <div class="chat-msg__txt">${escapeHtml(msg.text || '').replaceAll('\n', '<br>')}</div>
        </div>
      </div>`;
  }
  const conv = activeConv();
  const isLast = !!conv && index === conv.messages.length - 1;
  const hasText = (msg.text || '').length > 0;
  const body = hasText
    ? renderMarkdown(msg.text, { streaming, thinkKeyPrefix: String(msg.id), getThinkOpen: getThinkOpenState })
    : '';
  const caret = streaming ? '<span class="chat-caret"></span>' : '';
  // The caret has to sit inside the last block, or it lands on a line of its own.
  const bodyWithCaret = /<\/code><\/pre><\/div>$/.test(body)
    ? body.replace(/<\/code><\/pre><\/div>$/, `${caret}</code></pre></div>`)
    : /<\/(?:p|li)>(?:\s*<\/(?:ul|ol)>)*\s*$/.test(body)
    ? body.replace(/(<\/(?:p|li)>(?:\s*<\/(?:ul|ol)>)*\s*)$/, `${caret}$1`)
    : body + caret;
  const waiting = streaming && !hasText && !msg.status
    ? '<span class="chat-typing" aria-hidden="true"><span></span><span></span><span></span></span>'
    : '';
  // Live step of an answer still being generated ("narzędzie · search_web").
  // Only while streaming: once the turn settles the activity widget holds the record.
  const status = streaming && msg.status
    ? `<div class="chat-msg__status" role="status" aria-live="polite"><span class="chat-msg__status-dot"></span><span>${escapeHtml(msg.status)}</span></div>`
    : '';
  const settled = !streaming && !msg.failed;
  const chips = settled ? metaChips(msg) : '';
  const acts = streaming ? '' : `
    <span class="chat-msg__acts">
      <tf-button variant="ghost" size="sm" icon="copy" data-act="copy" aria-label="${escapeHtml(t('copy'))}" title="${escapeHtml(t('copy'))}"></tf-button>
      ${isLast ? `<tf-button variant="ghost" size="sm" icon="refresh" data-act="regenerate" aria-label="${escapeHtml(t('regenerate'))}" title="${escapeHtml(t('regenerate'))}"></tf-button>` : '<span class="chat-msg__acts-gap" aria-hidden="true"></span>'}
    </span>`;
  return `
    <div class="chat-msg ai${enterClass(msg)}" data-msg-id="${msg.id}">
      <div class="chat-msg__av" aria-hidden="true">${sprite('sparkles')}</div>
      <div class="chat-msg__body">
        <div class="chat-msg__who">${whoLine(msg)}</div>
        ${status}
        <div class="chat-msg__txt">${bodyWithCaret}${waiting}</div>
        ${msg.failed ? failureBox(msg) : ''}
        ${streaming || (!chips && !acts) ? '' : `<div class="chat-msg__meta">${chips}${acts}</div>`}
      </div>
    </div>`;
}

// ---- Height heuristics ---------------------------------------------------

// itemHeight is a heuristic (the virtualizer measures real heights once an item
// is on screen). Fences and <think> blocks get fixed-cost extras instead of
// per-segment monospace measurement.
function itemHeight(msg) {
  const inner = listWidth || 736;
  const isUser = msg.role === 'user';
  const colWidth = inner - AVATAR_AND_GAP_PX;
  const bubbleMax = isUser ? Math.min(USER_BUBBLE_MAX, colWidth * 0.8) - USER_BUBBLE_PADDING_PX : colWidth;
  const text = msg.text || '';

  let extra = 0;
  let measured = text;
  if (!isUser) {
    extra += Math.floor((text.match(/```/g) || []).length / 2) * FENCE_HEADER_PX;
    // A <think> block is collapsed in the DOM, so its text does not count toward
    // the height; only the chip does. `</think>` without a preceding `<think>`
    // means everything before the tag is thinking.
    let stripped = text;
    let blocks = 0;
    stripped = stripped.replace(/<think(?:ing)?>([\s\S]*?)<\/think(?:ing)?>/gi, () => { blocks += 1; return ''; });
    const lastClose = stripped.search(/<\/think(?:ing)?>[^<]*$/i);
    if (lastClose >= 0) {
      stripped = stripped.slice(stripped.indexOf('>', lastClose) + 1);
      blocks += 1;
    }
    extra += blocks * THINK_COLLAPSED_PX;
    measured = stripped;
  }
  const txt = measureItemHeight(measured || ' ', {
    font: getDefaultFont(),
    maxWidth: Math.max(80, bubbleMax),
    lineHeight: getDefaultLineHeight(),
  });
  const chrome = isUser ? 22 + 24 : 22 + 44;
  // + the 20px gap that is real padding-bottom on .vlist-item.
  return Math.max(64, txt + extra + chrome + 20);
}

// ---- Body: empty state / virtual list ------------------------------------

function computeInnerWidth(host) {
  const cs = window.getComputedStyle(host);
  const pl = parseFloat(cs.paddingLeft) || 0;
  const pr = parseFloat(cs.paddingRight) || 0;
  return Math.max(80, host.clientWidth - pl - pr);
}

// Delegated `toggle` listener on <details data-think-key>, bound once per host.
function ensureThinkToggleListener(host) {
  if (!host || host.dataset.thinkToggleBound === '1') return;
  host.dataset.thinkToggleBound = '1';
  host.addEventListener('toggle', (e) => {
    const det = e.target;
    if (!(det instanceof HTMLDetailsElement)) return;
    const key = det.getAttribute('data-think-key');
    if (key) thinkOpenState.set(key, det.open);
  }, true);
}

function destroyList() {
  if (vlist) { vlist.destroy(); vlist = null; }
}

function emptyStateMarkup() {
  const cards = [
    ['code', 'sug_code', 'code'],
    ['file', 'sug_doc', 'doc'],
    ['gpu', 'sug_infra', 'infra'],
    ['mic', 'sug_voice', 'voice'],
  ].map(([icon, key, id]) => `
    <tf-button wrap variant="ghost" icon="${icon}" data-suggest="${id}"><b>${escapeHtml(t(`${key}_cat`))}</b><i>${escapeHtml(t(`${key}_text`))}</i></tf-button>`).join('');
  return `
    <div class="chat-empty"><div>
      <div class="chat-empty__ico">${sprite('sparkles')}</div>
      <h2>${escapeHtml(t('empty_title'))}</h2>
      <p>${escapeHtml(t('empty_text'))}</p>
      <div class="chat-suggest">${cards}</div>
    </div></div>`;
}

// Shows the list of the active conversation, or the empty state when it has no
// messages yet (the list is mounted lazily by the first message).
function mountBody() {
  const host = byId('chat-body');
  if (!host) return;
  destroyList();
  const conv = activeConv();
  if (!conv || conv.messages.length === 0) {
    host.innerHTML = emptyStateMarkup();
    byId('chat-new-pill')?.classList.remove('visible');
    return;
  }
  host.innerHTML = '';
  mountVList(conv);
}

function mountVList(conv) {
  const host = byId('chat-body');
  if (!host) return;
  ensureThinkToggleListener(host);
  listWidth = computeInnerWidth(host);
  destroyList();
  vlist = createVirtualList(host, {
    items: conv.messages,
    pinToBottom: true,
    measureHeights: true,
    overscan: 10,
    getItemHeight: (_i, msg) => itemHeight(msg),
    renderItem: (i, msg) => renderMessage(i, msg),
    onScroll: (_top, _dist, { pinned }) => {
      if (pinned) byId('chat-new-pill')?.classList.remove('visible');
    },
  });
}

function remountIfWidthChanged() {
  const host = byId('chat-body');
  if (!host || !vlist) return;
  const w = computeInnerWidth(host);
  if (Math.abs(w - listWidth) > 1) {
    listWidth = w;
    vlist.refresh();
  }
}

function pushMessage(conv, msg) {
  // vlist.append shares the items reference with conv.messages; a separate
  // conv.messages.push would duplicate the entry.
  if (conv.mode === 'text' && conv.id === activeConvId) {
    if (!vlist) {
      conv.messages.push(msg);
      byId('chat-body').innerHTML = '';
      mountVList(conv);
    } else {
      vlist.append(msg);
    }
  } else {
    conv.messages.push(msg);
  }
  conv.updatedAt = Date.now();
  saveConversations();
  if (conv.mode === 'audio' && conv.id === activeConvId) audioView?.render(conv);
}

// Direct call (no rAF) — background tabs throttle rAF to <1Hz in Chrome, which
// would stall token rendering when the user switches away.
// Streamed text reaches storage while it arrives, so a reload mid-answer keeps
// the fragment the interrupted state promises.
let saveTimer = null;
function scheduleSave() {
  if (saveTimer) return;
  saveTimer = setTimeout(() => {
    saveTimer = null;
    saveConversations();
  }, STREAM_SAVE_MS);
}

function onStreamTick() {
  scheduleSave();
  const conv = activeConv();
  if (conv?.mode === 'audio') {
    audioView?.render(conv);
    return;
  }
  if (!vlist) return;
  const wasPinned = vlist.pinned;
  vlist.updateTail();
  const pill = byId('chat-new-pill');
  if (!pill) return;
  pill.classList.toggle('visible', !wasPinned);
}

// Re-renders every visible message: buttons that depend on position (the
// regenerate action sits on the newest answer only) need it after the list grew.
function refreshList() {
  vlist?.refresh();
}

// ---- Top bar: decode speed and TTFT --------------------------------------

function metricsFor(conv) {
  let m = metricsByConv.get(conv.id);
  if (!m) {
    m = new SessionMetrics();
    m.seed(conv.messages.map((msg) => msg.perf));
    metricsByConv.set(conv.id, m);
  }
  return m;
}

function tipRows(stats) {
  const rate = (v) => `${fmtRate(v, lang())} ${t('perf_decode')}`;
  return [
    [t('tip_now'), rate(stats.cur)],
    [t('tip_avg'), rate(stats.avg)],
    [t('tip_minmax'), `${fmtRate(stats.min, lang())} · ${fmtRate(stats.max, lang())}`],
    [t('tip_samples'), String(stats.n)],
  ].map(([k, v]) => `<div class="tip-row"><span>${escapeHtml(k)}</span><span>${escapeHtml(v)}</span></div>`).join('');
}

function updateMetrics() {
  const hero = byId('chat-decode');
  if (!hero) return;
  const conv = activeConv();
  const metrics = conv ? metricsFor(conv) : null;
  const stats = metrics ? metrics.stats() : null;

  byId('m-cur').textContent = stats ? fmtRate(stats.cur, lang()) : '—';
  const sub = byId('m-sub');
  if (!stats) {
    sub.innerHTML = '';
  } else {
    const vars = { avg: fmtRate(stats.avg, lang()), n: stats.n };
    sub.innerHTML = richText(t('metric_avg', vars));
  }
  byId('m-spark').innerHTML = sparkMarkup(metrics ? metrics.values : [], stats ? stats.avg : 0);

  const tip = byId('m-tip');
  const head = `<b>${escapeHtml(t('tip_title'))}</b> `;
  tip.innerHTML = stats
    ? `${head}${richText(t('tip_body'))}${tipRows(stats)}`
    : `${head}${richText(t('tip_empty'))}`;

  hero.classList.toggle('has-data', !!stats);
  byId('m-ttft').textContent = metrics?.ttftMs ? String(metrics.ttftMs) : '—';
  byId('m-ttft-unit').hidden = !metrics?.ttftMs;
}

// ---- Connection ----------------------------------------------------------

// What the UI does when the server link drops or returns. A generation in
// flight cannot continue (its stream died with the socket), so it is marked
// interrupted and offered for resume; the voice pipeline is paused, not stopped.
function applyConnection() {
  updateMetrics();
  if (audioPipeline) audioPipeline.setSuspended(conn.lost);
  syncAudioView();
  if (!conn.lost) return;
  if (turn) interruptTurn(turn);
}

// The stream died with the socket: what arrived stays, nothing is measured
// from a truncated answer and the user can resume.
function interruptTurn(tn) {
  abandonTurn(tn);
  discardTurnSamples(tn);
  tn.msg.streaming = false;
  tn.msg.status = '';
  tn.msg.failed = { kind: 'interrupted' };
  dropUnrecognizedUtterance(tn);
  tn.conv.updatedAt = Date.now();
  saveConversations();
  onStreamTick();
  refreshList();
  renderConvList();
  if (tn.kind === 'voice') {
    audioPipeline?.feedAssistantError();
    if (tn.conv.id === activeConvId) audioView?.render(tn.conv);
  }
}

// A voice turn that failed before any transcript arrived has nothing to show:
// the mic placeholder would read "recognizing…" forever.
function dropUnrecognizedUtterance(tn) {
  if (tn.kind !== 'voice' || tn.userMsg?.text !== MIC_PLACEHOLDER) return false;
  tn.conv.messages = tn.conv.messages.filter((m) => m !== tn.userMsg && m !== tn.msg);
  return true;
}

// The transport reports its own loss as a synthetic end-of-stream frame.
function isTransportLoss(end) {
  return end?._variant === 'StreamBody.Closed';
}

function onLifecycle(ev) {
  if (ev.type === 'open') {
    conn = { lost: false };
  } else if (ev.type === 'disconnected' || ev.type === 'close' || ev.type === 'reconnect-scheduled') {
    conn.lost = true;
  } else {
    return;
  }
  applyConnection();
}

// ---- Generation turns ----------------------------------------------------

function beginTurn(conv, msg, kind) {
  const meter = new DecodeMeter();
  const metrics = metricsFor(conv);
  const tn = { conv, msg, meter, sampler: null, unsub: null, cancelled: false, kind, pushed: 0 };
  tn.sampler = setInterval(() => {
    const rate = meter.sample();
    if (rate === null) return;
    metrics.push(rate);
    if (rate > 0) tn.pushed += 1;
    if (conv.id === activeConvId) updateMetrics();
  }, SAMPLE_INTERVAL_MS);
  turn = tn;
  setSendMode(true);
  return tn;
}

// An answer that did not finish says nothing about decode speed: the samples
// it fed the session series are taken back.
function discardTurnSamples(tn) {
  metricsFor(tn.conv).drop(tn.pushed);
  tn.pushed = 0;
}

function attachUnsub(tn, promise) {
  promise.then((unsub) => {
    tn.unsub = unsub;
    if (tn.cancelled) unsub();
  }).catch((err) => failTurn(tn, err));
}

// Stops listening to a turn's stream without touching its message.
function abandonTurn(tn) {
  tn.cancelled = true;
  clearInterval(tn.sampler);
  try { tn.unsub?.(); } catch { /* the socket may already be gone */ }
  if (turn === tn) turn = null;
  setSendMode(false);
}

function turnDelta(tn) {
  const first = tn.meter.firstAt === null;
  tn.meter.delta();
  if (first) {
    const metrics = metricsFor(tn.conv);
    metrics.setTtft(tn.meter.ttftMs);
    if (tn.conv.id === activeConvId) updateMetrics();
  }
}

// Records what the finished answer measured. The server's own numbers win over
// the browser's: they exclude network jitter between chunks.
function finishMetrics(tn, end) {
  const metrics = metricsFor(tn.conv);
  const serverTps = Number(end?.decodeTps) || 0;
  const serverTtft = Number(end?.ttftMs) || 0;
  const rate = serverTps > 0 ? serverTps : tn.meter.overall();
  const ttft = serverTtft > 0 ? serverTtft : tn.meter.ttftMs;
  if (!tn.meter.sampled && rate) metrics.push(rate);
  if (ttft) metrics.setTtft(ttft);
  tn.msg.perf = {
    decodeTps: rate || 0,
    ttftMs: ttft || 0,
    completionTokens: Number(end?.completionTokens) || 0,
    promptTokens: Number(end?.promptTokens) || 0,
    totalMs: Number(end?.totalMs) || 0,
  };
}

function settleTurn(tn) {
  clearInterval(tn.sampler);
  if (turn === tn) turn = null;
  setSendMode(false);
  tn.conv.updatedAt = Date.now();
  saveConversations();
  onStreamTick();
  renderConvList();
  updateMetrics();
  if (tn.conv.mode === 'audio' && tn.conv.id === activeConvId) audioView?.render(tn.conv);
}

function failTurn(tn, err) {
  if (tn.cancelled) return;
  clearInterval(tn.sampler);
  if (turn === tn) turn = null;
  setSendMode(false);
  tn.msg.streaming = false;
  tn.msg.status = '';
  const raw = String(err?.message || err || '');
  discardTurnSamples(tn);
  if (conn.lost) tn.msg.failed = { kind: 'interrupted' };
  else tn.msg.failed = { kind: /no model/i.test(raw) ? 'unavailable' : 'fault' };
  // Detail stays in the console: it can name a backend the UI must not show.
  console.warn('[chat] generation failed:', raw);
  if (dropUnrecognizedUtterance(tn)) toast(t('audio_stt_failed'), 'error');
  settleTurn(tn);
  if (tn.kind === 'voice' && audioPipeline) audioPipeline.feedAssistantError(err);
}

// The user pressed Stop: the server stream is closed (that cancels generation
// upstream) and what arrived so far stays as the answer.
// A stream that ended without a single token is a failed answer, not a finished one.
function emptyAnswer(tn) {
  discardTurnSamples(tn);
  tn.msg.failed = { kind: 'empty' };
  settleTurn(tn);
}

function stopTurn() {
  const tn = turn;
  if (!tn) return;
  abandonTurn(tn);
  tn.msg.streaming = false;
  tn.msg.status = '';
  tn.msg.stopped = true;
  finishMetrics(tn, null);
  if (tn.kind === 'voice' && audioPipeline) audioPipeline.abort();
  settleTurn(tn);
}

function composePrompt(userMsg) {
  if (!userMsg.attachments?.length) return userMsg.text;
  const files = userMsg.attachments.map((a) => `--- ${a.name} ---\n${a.content}`).join('\n\n');
  return `${userMsg.text}\n\n${files}`;
}

// The two tags stream_handlers.rs puts in front of a failed stream's error text.
// Exact names, so an answer that merely starts with "[Warning error] ..." stays an answer.
const STREAM_FAULT_PREFIX = /^\s*\[(?:routing|stream) error\]\s/;

function runTextTurn(conv, userMsg, assistantMsg) {
  if (!defaultChatFlow) {
    toast(t('unavailable'), 'error');
    return;
  }
  assistantMsg.streaming = true;
  const tn = beginTurn(conv, assistantMsg, 'text');
  tn.fresh = true;
  attachUnsub(tn, ApiBinary.subscribe(
    'chatStreamRequest',
    // conv.id is the flow session id — lets conversation_history / memory nodes
    // in Default Chat key off the conversation. modelId stays empty on purpose:
    // the models come from the flow's blocks.
    { modelId: '', userMessage: composePrompt(userMsg), flowId: defaultChatFlow.id, sessionId: conv.id },
    {
      onChunk: (body) => {
        if (tn.cancelled || body._variant !== 'ChatStreamChunkBody' || !body.delta) return;
        // Its text names internals, so it is a turn failure, never part of the answer.
        if (STREAM_FAULT_PREFIX.test(body.delta)) {
          failTurn(tn, new Error(body.delta.trim()));
          abandonTurn(tn);
          return;
        }
        turnDelta(tn);
        if (tn.fresh) {
          assistantMsg.text = '';
          tn.fresh = false;
        }
        assistantMsg.text += body.delta;
        conv.updatedAt = Date.now();
        onStreamTick();
      },
      onEnd: (end) => {
        if (tn.cancelled) return;
        if (isTransportLoss(end)) {
          interruptTurn(tn);
          return;
        }
        assistantMsg.streaming = false;
        assistantMsg.status = '';
        // ChatStreamEnd.text is the whole text from the server — used when the
        // joined deltas are empty (lost chunks).
        if (tn.fresh && typeof end?.text === 'string' && end.text !== '') {
          assistantMsg.text = end.text;
          tn.fresh = false;
        }
        if (tn.fresh) {
          emptyAnswer(tn);
          return;
        }
        finishMetrics(tn, end);
        settleTurn(tn);
      },
      onError: (err) => failTurn(tn, err),
    },
  ));
}

// A voice utterance goes to the flow engine as one FlowInvoke: audio → STT →
// LLM → TTS, answered with interleaved text and audio chunks.
function sendVoiceUtterance(wav, sampleRate) {
  const conv = ensureActiveConv();
  if (!defaultChatFlow) {
    toast(t('unavailable'), 'error');
    return;
  }
  if (turn) abandonTurn(turn);

  // The user bubble starts as a mic placeholder; the flow's `transcript` chunk
  // (emitted before the first assistant token) replaces it with the STT text.
  const userMsg = { id: nextMsgId++, role: 'user', text: MIC_PLACEHOLDER, ts: Date.now(), via: 'voice' };
  pushMessage(conv, userMsg);
  const assistantMsg = { id: nextMsgId++, role: 'assistant', text: '', ts: Date.now(), streaming: true, via: 'voice' };
  pushMessage(conv, assistantMsg);

  const tn = beginTurn(conv, assistantMsg, 'voice');
  tn.userMsg = userMsg;
  attachUnsub(tn, ApiBinary.subscribe(
    'flowInvokeRequest',
    {
      // Same flow as text chat; outputAudio switches the TTS block on.
      flowId: defaultChatFlow.id,
      model: '',
      serviceType: 'chat',
      mime: 'audio/wav',
      sampleRate,
      audio: wav,
      language: conv.audioConfig?.language || lang(),
      sessionId: conv.id,
      outputAudio: true,
    },
    {
      onChunk: (body) => {
        if (tn.cancelled) return;
        if (body._variant === 'FlowInvokeChunkBody.Transcript') {
          const text = (body.text || '').trim();
          if (!text) return;
          userMsg.text = text;
          if (!conv.renamed && !conv.title) conv.title = titleFrom(text);
          conv.updatedAt = Date.now();
          saveConversations();
          renderConvList();
          audioView?.render(conv);
        } else if (body._variant === 'FlowInvokeChunkBody.Text') {
          if (!body.delta) return;
          turnDelta(tn);
          assistantMsg.text += body.delta;
          conv.updatedAt = Date.now();
          onStreamTick();
        } else if (body._variant === 'FlowInvokeChunkBody.Audio') {
          audioPipeline?.playAudioChunk(body.bytes, body.mime);
        }
      },
      onEnd: (end) => {
        if (tn.cancelled) return;
        if (isTransportLoss(end)) {
          interruptTurn(tn);
          return;
        }
        assistantMsg.streaming = false;
        if (end?.error) {
          failTurn(tn, new Error(end.error));
          return;
        }
        // FlowInvokeEnd.text is authoritative: joined deltas can end early while
        // the audio is still playing.
        if (typeof end?.text === 'string' && end.text.length > 0) assistantMsg.text = end.text;
        if (assistantMsg.text === '') {
          emptyAnswer(tn);
          audioPipeline?.finishResponse();
          return;
        }
        finishMetrics(tn, null);
        settleTurn(tn);
        audioPipeline?.finishResponse();
      },
      onError: (err) => failTurn(tn, err),
    },
  ));
}

// Resends the question of an answer and replaces that answer in place, so the
// transcript never carries two answers to one question.
function regenerate(msgId) {
  if (turn || conn.lost) return;
  const conv = activeConv();
  if (!conv) return;
  const idx = conv.messages.findIndex((m) => m.id === msgId);
  if (idx < 1 || idx !== conv.messages.length - 1) return;
  const answer = conv.messages[idx];
  const question = conv.messages[idx - 1];
  if (answer.role !== 'assistant' || question.role !== 'user' || question.text === MIC_PLACEHOLDER) return;
  // The old fragment stays until the new stream delivers its first token, so a
  // resume that fails leaves exactly what the failure box promised to keep.
  answer.perf = undefined;
  answer.failed = undefined;
  answer.stopped = false;
  answer.ts = Date.now();
  answer.via = 'text';
  runTextTurn(conv, question, answer);
  refreshList();
  vlist?.scrollToBottom();
}

// ---- Sending -------------------------------------------------------------

function ensureActiveConv() {
  const existing = activeConv();
  if (existing) return existing;
  const conv = newConversation();
  conversations.push(conv);
  activeConvId = conv.id;
  saveConversations();
  rebindAgentActivity();
  renderConvList();
  updateMetrics();
  return conv;
}

function currentInputValue() {
  return byId('chat-input')?.value || '';
}

function setInputValue(value) {
  const input = byId('chat-input');
  if (input) input.value = value;
  updateInputCounter();
}

function sendMessage() {
  if (turn || conn.lost) return;
  const text = currentInputValue().trim();
  if (!text) return;
  if (!defaultChatFlow) {
    toast(t('unavailable'), 'error');
    return;
  }
  const conv = ensureActiveConv();
  if (!conv.renamed && !conv.title && conv.messages.length === 0) {
    conv.title = titleFrom(text);
  }
  const userMsg = { id: nextMsgId++, role: 'user', text, ts: Date.now(), via: 'text' };
  if (pendingFiles.length) userMsg.attachments = pendingFiles;
  pendingFiles = [];
  renderAttachments();
  setInputValue('');
  pushMessage(conv, userMsg);
  const assistantMsg = { id: nextMsgId++, role: 'assistant', text: '', ts: Date.now(), streaming: true, via: 'text' };
  pushMessage(conv, assistantMsg);
  renderConvList();
  runTextTurn(conv, userMsg, assistantMsg);
  refreshList();
  vlist?.scrollToBottom();
}

// One button is both Send and Stop: while a turn runs it stops it.
function setSendMode(generating) {
  const btn = byId('chat-send');
  if (!btn) return;
  btn.setAttribute('icon', generating ? 'stop' : 'send');
  const label = t(generating ? 'stop' : 'send');
  btn.setAttribute('aria-label', label);
  btn.setAttribute('title', label);
  btn.dataset.generating = String(generating);
}

function onSendClick() {
  if (turn) stopTurn();
  else sendMessage();
}

function updateInputCounter() {
  const counter = byId('chat-input-counter');
  if (!counter) return;
  const len = currentInputValue().length;
  counter.textContent = t('counter', { len, max: MAX_INPUT_CHARS });
  counter.classList.toggle('warn', len > MAX_INPUT_CHARS * 0.75);
}

// ---- Attachments ---------------------------------------------------------

function renderAttachments() {
  const host = byId('chat-attachments');
  if (!host) return;
  host.innerHTML = pendingFiles
    .map((f, i) => `<tf-chip icon="file" mono removable data-file="${i}">${escapeHtml(f.name)}</tf-chip>`)
    .join('');
}

async function onFilesPicked(files) {
  for (const file of Array.from(files || [])) {
    if (pendingFiles.length >= ATTACH_MAX_FILES) {
      toast(t('attach_limit', { count: ATTACH_MAX_FILES }), 'error');
      break;
    }
    if (file.size > ATTACH_MAX_BYTES) {
      toast(t('attach_too_big', { name: file.name, kb: ATTACH_MAX_BYTES / 1024 }), 'error');
      continue;
    }
    let content;
    try {
      content = await file.text();
    } catch {
      toast(t('attach_read_failed', { name: file.name }), 'error');
      continue;
    }
    // A NUL byte means a binary file; its decoded text would only be noise in the prompt.
    if (content.includes('\u0000')) {
      toast(t('attach_binary', { name: file.name }), 'error');
      continue;
    }
    pendingFiles.push({ name: file.name, size: file.size, content });
  }
  renderAttachments();
  byId('chat-attach')?.clear?.();
}

// ---- Click delegation inside messages ------------------------------------

function onBodyClick(e) {
  const copyCode = e.target.closest('.copy-btn');
  if (copyCode) {
    let plain = '';
    try { plain = decodeURIComponent(escape(atob(copyCode.dataset.code || ''))); } catch { plain = ''; }
    if (plain) copyText(plain);
    return;
  }
  const suggest = e.target.closest('[data-suggest]');
  if (suggest) {
    onSuggestion(suggest.dataset.suggest);
    return;
  }
  const act = e.target.closest('[data-act]');
  if (!act) return;
  const msgId = Number(act.closest('.chat-msg')?.dataset.msgId);
  const msg = activeConv()?.messages.find((m) => m.id === msgId);
  if (!msg) return;
  if (act.dataset.act === 'copy') copyText(msg.text || '');
  else if (act.dataset.act === 'regenerate' || act.dataset.act === 'resume') regenerate(msg.id);
}

function copyText(text) {
  const done = () => toast(t('copied'), 'info');
  const fail = () => toast(t('copy_failed'), 'error');
  if (!navigator.clipboard?.writeText) { fail(); return; }
  navigator.clipboard.writeText(text).then(done, fail);
}

function onSuggestion(id) {
  if (id === 'voice') {
    switchMode('audio');
    return;
  }
  setInputValue(t(`sug_${id}_text`));
  byId('chat-input')?.focus();
}

// ---- Conversation actions ------------------------------------------------

function pruneEmptyConversations(exceptId) {
  const before = conversations.length;
  conversations = conversations.filter((c) => c.id === exceptId || c.messages.length > 0 || c.renamed);
  if (conversations.length !== before) saveConversations();
}

function startNewConversation() {
  const current = activeConv();
  // An empty conversation already is a new one.
  if (current && current.messages.length === 0 && current.mode === 'text') {
    byId('chat-input')?.focus();
    closeDrawer();
    return;
  }
  const conv = newConversation();
  conversations.push(conv);
  activateConversation(conv.id);
  byId('chat-input')?.focus();
}

function selectConversation(id) {
  if (id === activeConvId) {
    closeDrawer();
    return;
  }
  activateConversation(id);
}

function activateConversation(id) {
  if (turn) stopTurn();
  stopAudioPipeline();
  activeConvId = id;
  pruneEmptyConversations(id);
  rebindAgentActivity();
  renderConvList();
  applyMode(activeConv());
  updateMetrics();
  closeDrawer();
}

async function renameConversation(id) {
  const conv = conversations.find((c) => c.id === id);
  if (!conv) return;
  const body = document.createElement('div');
  body.innerHTML = `<tf-input id="chat-rename-input" maxlength="120" aria-label="${escapeHtml(t('rename_label'))}" label="${escapeHtml(t('rename_label'))}"></tf-input>`;
  const input = body.querySelector('tf-input');
  input.setAttribute('value', conv.title || '');
  const footer = document.createElement('div');
  footer.innerHTML = `
    <tf-button variant="secondary" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    <tf-button variant="primary" data-action="save" id="chat-rename-save">${escapeHtml(t('rename_save'))}</tf-button>`;
  body.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.isComposing) {
      e.preventDefault();
      footer.querySelector('[data-action="save"]').click();
    }
  });
  const closed = TfWindow.open({
    title: t('rename_title'),
    icon: 'edit',
    width: 420,
    modal: true,
    buttons: 'close',
    body,
    footer,
  });
  requestAnimationFrame(() => input.focus?.());
  const { action } = await closed;
  if (action !== 'save') return;
  const next = String(input.value || '').trim().slice(0, 120);
  if (!next || next === conv.title) return;
  conv.title = next;
  conv.renamed = true;
  saveConversations();
  renderConvList();
}

async function deleteConversation(id) {
  const conv = conversations.find((c) => c.id === id);
  if (!conv) return;
  const ok = await TfWindow.confirm({
    title: t('delete_title'),
    message: t('delete_message', { title: convTitle(conv) }),
    confirmLabel: t('delete_confirm'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;
  const wasActive = id === activeConvId;
  if (wasActive) {
    if (turn) stopTurn();
    stopAudioPipeline();
  }
  conversations = conversations.filter((c) => c.id !== id);
  metricsByConv.delete(id);
  if (wasActive) {
    activeConvId = [...conversations].sort((a, b) => b.updatedAt - a.updatedAt)[0]?.id || null;
    rebindAgentActivity();
  }
  saveConversations();
  renderConvList();
  if (wasActive) {
    applyMode(activeConv());
    updateMetrics();
  }
  toast(t('delete_done'), 'info');
}

function onConvMenuAction(action) {
  const id = menuConvId;
  if (!id) return;
  if (action === 'rename') renameConversation(id);
  else if (action === 'delete') deleteConversation(id);
}

function onConvListClick(e) {
  const more = e.target.closest('[data-conv-more]');
  if (more) {
    e.stopPropagation();
    const menu = byId('chat-conv-menu');
    const id = more.dataset.convMore;
    if (menuConvId === id && menu.hasAttribute('open')) {
      menu.close();
      return;
    }
    menuConvId = id;
    menu.anchor = more;
    menu.open();
    // A keyboard activation synthesizes a click with no pointer: focus has to
    // enter the menu for the arrows to work.
    if (e.detail === 0) menu.focusFirst();
    return;
  }
  const item = e.target.closest('.conv-item');
  if (item) selectConversation(item.dataset.convId);
}

function onConvListKey(e) {
  if (e.key !== 'Enter' && e.key !== ' ') return;
  const item = e.target.closest?.('.conv-item');
  if (!item || e.target !== item) return;
  e.preventDefault();
  selectConversation(item.dataset.convId);
}

// ---- Audio mode ----------------------------------------------------------

function switchMode(target) {
  const conv = ensureActiveConv();
  if (conv.mode === target) {
    syncModeSwitch(target);
    return;
  }
  if (target === 'audio' && !defaultChatFlow) {
    toast(t('unavailable'), 'error');
    syncModeSwitch(conv.mode);
    return;
  }
  if (turn) stopTurn();
  conv.mode = target;
  conv.updatedAt = Date.now();
  saveConversations();
  applyMode(conv);
  renderConvList();
}

function syncModeSwitch(mode) {
  const sw = byId('chat-mode');
  if (sw && sw.value !== mode) sw.value = mode;
}

// Swaps the body between the text view and the audio stage for the conversation.
function applyMode(conv) {
  const body = byId('chat-body');
  const col = byId('chat-col');
  if (!body || !col) return;
  const mode = conv?.mode || 'text';
  const activity = byId('chat-agent-activity');
  if (activity) activity.variant = mode === 'audio' ? 'chat-audio' : 'chat';
  col.classList.toggle('is-audio', mode === 'audio');
  body.classList.toggle('audio-mode', mode === 'audio');
  syncModeSwitch(mode);
  destroyList();
  destroyAudioView();
  if (mode === 'audio') {
    body.innerHTML = '';
    audioView = createAudioView(body, {
      onExit: () => switchMode('text'),
      onMic: onDockMic,
      onPause: onDockPause,
      onVolume: onDockVolume,
    });
    faceHandle = FaceBackground.embed(audioView.faceHost, { fullscreen: true, track: 'pointer' });
    faceHandle.setMode('idle');
    audioUi = { state: 'idle', text: '' };
    audioView.render(conv);
    syncAudioView();
  } else {
    stopAudioPipeline();
    mountBody();
  }
}

function destroyAudioView() {
  if (faceHandle) { faceHandle.destroy(); faceHandle = null; }
  if (audioView) { audioView.destroy(); audioView = null; }
}

// Pushes the pipeline/link state to the stage: pill, dock and header chip.
function syncAudioView() {
  if (!audioView) return;
  audioView.setState(audioUi.state, audioUi.text);
  audioView.setChip(audioPipeline ? 'live' : 'idle');
  audioView.setDock({
    started: !!audioPipeline,
    muted: !!audioPipeline?.isMuted(),
    speakerMuted: !!audioPipeline?.speakerMuted,
  });
}

function setAudioUi(state, text = '') {
  audioUi = { state, text };
  syncAudioView();
}

async function onDockMic() {
  if (!audioPipeline) {
    await startAudioPipeline();
    return;
  }
  audioPipeline.mute(!audioPipeline.isMuted());
  syncAudioView();
}

function onDockPause() {
  if (!audioPipeline) return;
  // Cuts what is being said or generated; the pipeline keeps listening.
  if (turn) stopTurn();
  audioPipeline.abort();
}

function onDockVolume() {
  if (!audioPipeline) return;
  const muted = audioPipeline.toggleSpeaker();
  if (muted) toast(t('audio_speaker_muted'), 'info');
  syncAudioView();
}

async function startAudioPipeline() {
  audioErrorToasted = false;
  if (audioPipeline) return;
  const conv = activeConv();
  if (!conv || conv.mode !== 'audio' || !faceHandle) return;
  if (!defaultChatFlow) {
    toast(t('unavailable'), 'error');
    return;
  }
  // The transcription language follows the interface language.
  conv.audioConfig.language = lang();
  try {
    audioPipeline = new AudioPipeline({
      conv,
      faceHandle,
      i18n: I18n,
      onUtteranceAudio: (wav, sampleRate) => {
        if (!wav || wav.length === 0) {
          toast(t('audio_empty_transcript'), 'info');
          return;
        }
        sendVoiceUtterance(wav, sampleRate);
      },
      onStateChange: (state) => {
        const map = { idle: 'idle', listening: 'listen', transcribing: 'think', thinking: 'think', speaking: 'speak', error: 'idle' };
        const ui = map[state] || 'idle';
        if (ui !== 'speak') audioView?.stopSpeech();
        setAudioUi(ui, state === 'transcribing' ? t('audio_state_transcribing') : '');
      },
      onVadChange: (active) => audioView?.setUserSpeaking(active),
      onPlaybackChunk: (info) => {
        const c = activeConv();
        const last = c?.messages[c.messages.length - 1];
        if (last?.role === 'assistant') audioView?.playChunk(info, last);
      },
      onError: (err) => {
        console.warn('[audio]', err);
        notifyAudioError();
      },
      bargeInAbort: () => {
        // The pipeline cut the TTS because the user started talking: the
        // generation behind it has to stop too.
        if (turn) stopTurn();
      },
    });
    await audioPipeline.start();
    audioPipeline.setSuspended(conn.lost);
    setAudioUi('listen');
  } catch (err) {
    audioPipeline = null;
    syncAudioView();
    if (err?.name === 'NotAllowedError') toast(t('audio_mic_denied'), 'error');
    else if (err?.name === 'NotFoundError') toast(t('audio_no_mic'), 'error');
    else {
      console.warn('[audio]', err);
      toast(t('audio_error'), 'error');
    }
  }
}

// A failing decoder reports once per chunk; one toast per session is enough,
// and the browser's own wording (English, names internals) stays in the console.
let audioErrorToasted = false;
function notifyAudioError() {
  if (audioErrorToasted) return;
  audioErrorToasted = true;
  toast(t('audio_error'), 'error');
}

function stopAudioPipeline() {
  if (!audioPipeline) return;
  try { audioPipeline.stop(); } catch { /* ignore */ }
  audioPipeline = null;
  audioUi = { state: 'idle', text: '' };
  syncAudioView();
}

// ---- Agent activity ------------------------------------------------------

// (Re)subscribe the agent-activity widget to the active conversation's session
// scope: the conversation id is the flow `session_id` background runs publish under.
function rebindAgentActivity() {
  if (agentActivityTeardown) { agentActivityTeardown(); agentActivityTeardown = null; }
  const widget = byId('chat-agent-activity');
  if (!widget || !activeConvId) return;
  agentActivityTeardown = attachAgentActivity(widget, activeConvId, {
    // Mirror the live step into the answer still streaming, so a slow local
    // model shows "narzędzie · search_web" instead of an empty bubble.
    onStatus: (status) => {
      const streamingMsg = activeConv()?.messages?.find((m) => m.streaming);
      if (!streamingMsg || streamingMsg.status === status) return;
      streamingMsg.status = status;
      onStreamTick();
    },
  });
}

// ---- Screen --------------------------------------------------------------

function modeOption(value, icon) {
  return `<option value="${value}" icon="${icon}">${escapeHtml(t(`mode_${value}`))}</option>`;
}

const ChatScreen = {
  get title() { return t('title'); },

  render() {
    return `
      <div class="chat-root" id="chat-root">
        <tf-menu id="chat-conv-menu" placement="bottom-end">
          <tf-menu-item action="rename" icon="edit">${escapeHtml(t('menu_rename'))}</tf-menu-item>
          <tf-menu-item action="delete" icon="trash" danger>${escapeHtml(t('menu_delete'))}</tf-menu-item>
        </tf-menu>
        <div class="chat-cq">
          <div class="chat-shell" id="chat-shell">
            <aside class="conv-panel" aria-label="${escapeHtml(t('conv_aria'))}">
              <div class="conv-top">
                <tf-button variant="primary" size="sm" icon="plus" id="chat-new">${escapeHtml(t('new_conversation'))}</tf-button>
                <tf-searchbox id="chat-search" size="sm" placeholder="${escapeHtml(t('search_placeholder'))}" debounce="200"></tf-searchbox>
              </div>
              <div class="conv-list" id="chat-conv-list"></div>
              <div class="conv-foot" id="chat-conv-foot"></div>
            </aside>
            <div class="conv-scrim" id="chat-scrim"></div>
            <section class="chat-col" id="chat-col">
              <header class="chat-top">
                <tf-tooltip block align="start" side="bottom" delay="120">
                  <div class="chat-metric is-decode" id="chat-decode" tabindex="0">
                    <div class="chat-metric__k">${sprite('gauge')}<span>${escapeHtml(t('metric_decode'))}</span><span class="lbl-full"> · ${escapeHtml(t('metric_scope'))}</span><span class="chat-metric__info">${sprite('info')}</span></div>
                    <div class="chat-metric__v"><b id="m-cur">—</b><small>${escapeHtml(t('perf_decode'))}</small></div>
                    <div class="chat-metric__sub" id="m-sub"></div>
                    <svg class="chat-metric__spark" id="m-spark" viewBox="0 0 64 22" aria-hidden="true"></svg>
                  </div>
                  <div slot="content" class="chat-tip" id="m-tip"></div>
                </tf-tooltip>
                <div class="chat-metric" id="chat-ttft" tabindex="0" title="${escapeHtml(t('metric_ttft_hint'))}">
                  <div class="chat-metric__k">${escapeHtml(t('perf_ttft'))}</div>
                  <div class="chat-metric__v"><b id="m-ttft">—</b><small id="m-ttft-unit" hidden>ms</small></div>
                </div>
                <div class="top-actions">
                  <tf-segmented id="chat-mode" size="md" value="text" aria-label="${escapeHtml(t('mode_aria'))}">
                    ${modeOption('text', 'message')}${modeOption('audio', 'mic')}
                  </tf-segmented>
                </div>
              </header>
              <div class="chat-body" id="chat-body"></div>
              <div class="chat-new-pill" id="chat-new-pill">${sprite('chevron-down')}<span>${escapeHtml(t('new_messages'))}</span></div>
              <div class="chat-composer-wrap" id="chat-composer-wrap">
                <div class="chat-activity"><tf-agent-activity id="chat-agent-activity" variant="chat"></tf-agent-activity></div>
                <div class="chat-attachments" id="chat-attachments"></div>
                <div class="chat-composer" id="chat-composer">
                  <tf-file-input id="chat-attach" variant="icon" icon="paperclip" multiple accept="${ATTACH_ACCEPT}"
                    label="${escapeHtml(t('attach'))}"></tf-file-input>
                  <tf-textarea id="chat-input" autogrow rows="1" maxlength="${MAX_INPUT_CHARS}"
                    aria-label="${escapeHtml(t('placeholder'))}" placeholder="${escapeHtml(t('placeholder'))}"></tf-textarea>
                  <tf-button variant="primary" size="sm" icon="send" id="chat-send" aria-label="${escapeHtml(t('send'))}" title="${escapeHtml(t('send'))}"></tf-button>
                </div>
                <div class="chat-hints" id="chat-hints">
                  <tf-button class="conv-opener" id="chat-conv-open" variant="ghost" size="sm" icon="message"
                    aria-label="${escapeHtml(t('conv_open'))}" title="${escapeHtml(t('conv_open'))}">${escapeHtml(t('conv_open'))}</tf-button>
                  <span><kbd>Enter</kbd> ${escapeHtml(t('hint_send'))}</span>
                  <span><kbd>Shift</kbd>+<kbd>Enter</kbd> ${escapeHtml(t('hint_newline'))}</span>
                  <span class="spacer"></span>
                  <span class="counter" id="chat-input-counter"></span>
                </div>
              </div>
            </section>
          </div>
        </div>
      </div>
    `;
  },

  async mount() {
    conversations = loadConversations();
    migrateConversations(conversations);
    conversations.sort((a, b) => b.updatedAt - a.updatedAt);
    activeConvId = conversations[0]?.id || null;
    let maxId = 0;
    for (const c of conversations) for (const m of c.messages) if (m.id > maxId) maxId = m.id;
    nextMsgId = maxId + 1;
    metricsByConv.clear();
    pendingFiles = [];
    turn = null;
    conn = { lost: !ApiBinary.isConnected() };

    // Avatar initials of the signed-in user, as the app shell shows them.
    ApiBinary.one('authMeRequest').then((me) => {
      userInitials = (me?.username ?? '?').slice(0, 2).toUpperCase();
      refreshList();
    }).catch(() => {});

    // Chat runs on ONE flow — the seeded Default Chat. Resolve it by its stable
    // id; the `isDefault` flag is the fallback for an installation whose admin
    // moved the default onto another flow.
    try {
      const flows = (await ApiBinary.list('flowListRequest', { arrayKey: 'flows' })) || [];
      defaultChatFlow = flows.find((f) => String(f.id) === DEFAULT_CHAT_FLOW_ID)
        || flows.find((f) => f.isDefault)
        || null;
    } catch {
      defaultChatFlow = null;
    }
    if (!defaultChatFlow) toast(t('unavailable'), 'error');

    renderConvList();
    applyMode(activeConv());
    updateInputCounter();
    updateMetrics();
    setSendMode(false);

    byId('chat-mode')?.addEventListener('change', (e) => switchMode(e.detail?.value === 'audio' ? 'audio' : 'text'));

    // Esc closes the conversation drawer first, then returns audio mode to text —
    // the keyboard way out for anyone who does not find the end-call button.
    escKeyHandler = (e) => {
      if (e.key !== 'Escape' || document.querySelector('tf-window')) return;
      if (byId('chat-shell')?.classList.contains('conv-open')) closeDrawer();
      else if (activeConv()?.mode === 'audio') switchMode('text');
    };
    document.addEventListener('keydown', escKeyHandler);

    // A reload does not wait for the throttled save of the answer being streamed.
    pageHideHandler = () => { if (turn) saveConversations(); };
    window.addEventListener('pagehide', pageHideHandler);

    // Push-to-talk — Space in audio mode (outside text fields) overrides the VAD:
    // held = speech, released = end of utterance. Helps in noisy rooms where the
    // adaptive threshold fails.
    spaceKeydownHandler = (e) => {
      if (e.key !== ' ' && e.code !== 'Space') return;
      if (activeConv()?.mode !== 'audio') return;
      const tgt = e.target;
      if (tgt && (tgt.tagName === 'INPUT' || tgt.tagName === 'TEXTAREA' || tgt.isContentEditable)) return;
      if (spaceHeld) return;
      spaceHeld = true;
      audioPipeline?.pushToTalkStart();
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
    byId('chat-new')?.addEventListener('click', startNewConversation);
    const list = byId('chat-conv-list');
    list?.addEventListener('click', onConvListClick);
    list?.addEventListener('keydown', onConvListKey);
    byId('chat-conv-menu')?.addEventListener('action', (e) => onConvMenuAction(e.detail?.action));
    byId('chat-conv-open')?.addEventListener('click', () => byId('chat-shell')?.classList.add('conv-open'));
    byId('chat-scrim')?.addEventListener('click', closeDrawer);

    byId('chat-new-pill')?.addEventListener('click', () => {
      vlist?.scrollToBottom();
      byId('chat-new-pill')?.classList.remove('visible');
    });

    byId('chat-send')?.addEventListener('click', onSendClick);
    byId('chat-attach')?.addEventListener('change', (e) => onFilesPicked(e.detail?.files));
    byId('chat-attachments')?.addEventListener('remove', (e) => {
      const idx = Number(e.target.closest('[data-file]')?.dataset.file);
      if (Number.isInteger(idx)) {
        pendingFiles.splice(idx, 1);
        renderAttachments();
      }
    });

    // Composer keymap: bare Enter sends, Shift/Alt+Enter inserts a newline,
    // Cmd/Ctrl+Enter is kept as a power-user alias. IME composition passes
    // through so CJK input does not send mid-compose.
    byId('chat-input')?.addEventListener('tf-keydown', (e) => {
      const { key, shiftKey, altKey, original } = e.detail;
      if (original?.isComposing || key !== 'Enter' || shiftKey || altKey) return;
      original?.preventDefault();
      sendMessage();
    });
    byId('chat-input')?.addEventListener('input', updateInputCounter);

    const body = byId('chat-body');
    body?.addEventListener('click', onBodyClick);
    if (body && typeof ResizeObserver !== 'undefined') {
      bodyResizeObserver = new ResizeObserver(() => remountIfWidthChanged());
      bodyResizeObserver.observe(body);
    }

    offLifecycle = ApiBinary.onLifecycle(onLifecycle);
    rebindAgentActivity();
  },

  async unmount() {
    if (turn) {
      abandonTurn(turn);
      saveConversations();
    }
    if (saveTimer) { clearTimeout(saveTimer); saveTimer = null; }
    if (pageHideHandler) { window.removeEventListener('pagehide', pageHideHandler); pageHideHandler = null; }
    if (agentActivityTeardown) { agentActivityTeardown(); agentActivityTeardown = null; }
    destroyList();
    stopAudioPipeline();
    destroyAudioView();
    if (offLifecycle) { offLifecycle(); offLifecycle = null; }
    if (bodyResizeObserver) { bodyResizeObserver.disconnect(); bodyResizeObserver = null; }
    if (escKeyHandler) { document.removeEventListener('keydown', escKeyHandler); escKeyHandler = null; }
    if (spaceKeydownHandler) { document.removeEventListener('keydown', spaceKeydownHandler); spaceKeydownHandler = null; }
    if (spaceKeyupHandler) { document.removeEventListener('keyup', spaceKeyupHandler); spaceKeyupHandler = null; }
  },
};

export default ChatScreen;
