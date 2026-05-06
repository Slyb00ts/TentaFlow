// =============================================================================
// File: lib/md-lite.js — minimal markdown renderer for chat messages.
// Supports: <think>...</think> blocks, code fences (```lang[:filename]),
// inline `code`, **bold**, *italic*, paragraphs, line breaks.
// All non-markup text is HTML-escaped. Output is a sanitized HTML string.
// =============================================================================

import { escapeHtml } from '/js/utils.js';

// btoa cannot encode raw UTF-16 directly; this round-trip preserves bytes.
function b64encode(str) {
  try {
    return btoa(unescape(encodeURIComponent(str)));
  } catch {
    return '';
  }
}

function renderThinkingBlock(inner, isOpen, key) {
  const charCount = inner.length;
  const escaped = escapeHtml(inner).replaceAll('\n', '<br>');
  const openAttr = isOpen ? ' open' : '';
  const keyAttr = key ? ` data-think-key="${escapeHtml(key)}"` : '';
  return `<details class="thinking"${openAttr}${keyAttr}>` +
    `<summary class="thinking-head">` +
      `<svg class="icon icon-think" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">` +
        `<path d="M12 2a7 7 0 0 0-4 12.7V17a2 2 0 0 0 2 2h4a2 2 0 0 0 2-2v-2.3A7 7 0 0 0 12 2z"/>` +
        `<path d="M9 22h6"/>` +
      `</svg>` +
      `<span class="label">Thinking</span>` +
      `<span class="meta">${charCount} chars</span>` +
      `<svg class="icon chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="6 9 12 15 18 9"/></svg>` +
    `</summary>` +
    `<div class="thinking-body">${escaped}</div>` +
  `</details>`;
}

function renderCodeBlock(lang, filename, code) {
  const escaped = escapeHtml(code);
  const langLabel = lang ? escapeHtml(lang) : 'text';
  const fileLabel = filename ? `<span class="filename mono">${escapeHtml(filename)}</span>` : '';
  const encoded = b64encode(code);
  return `<div class="code-block">` +
    `<div class="code-head">` +
      `<span class="lang mono">${langLabel}</span>` +
      fileLabel +
      `<div class="actions">` +
        `<button type="button" class="icon-btn copy-btn" data-code="${encoded}" title="Kopiuj">` +
          `<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>` +
          `<span>Kopiuj</span>` +
        `</button>` +
      `</div>` +
    `</div>` +
    `<pre><code>${escaped}</code></pre>` +
  `</div>`;
}

// Replace placeholder tokens in already-escaped HTML with rendered HTML.
// Placeholders use ASCII Unit Separator () so they survive escapeHtml.
function withPlaceholders(text, kind, makeReplacement) {
  const slots = [];
  const reText = text.replace(makeReplacement.regex, (...args) => {
    const idx = slots.length;
    slots.push(makeReplacement.handler(...args));
    return `${kind}${idx}`;
  });
  return { text: reText, slots };
}

function applyInlineMarkdown(escapedHtml) {
  // Inline code first — greedy fence avoidance not needed because we ran
  // fence extraction earlier. Backticks are already escaped to &#x60;? No —
  // escapeHtml leaves backticks alone, so we can match them directly.
  let out = escapedHtml.replace(/`([^`\n]+)`/g, (_m, code) => `<code class="inline">${code}</code>`);
  // Bold (**x**) — non-greedy, must not span newlines.
  out = out.replace(/\*\*([^*\n][^\n]*?)\*\*/g, (_m, inner) => `<strong>${inner}</strong>`);
  // Italic (*x*) — single asterisk, must not start with whitespace.
  out = out.replace(/(^|[^*])\*([^*\s][^*\n]*?)\*(?!\*)/g, (_m, pre, inner) => `${pre}<em>${inner}</em>`);
  return out;
}

function paragraphize(escapedHtml) {
  // Split on blank lines, wrap each chunk in <p>; single \n inside chunk → <br>.
  const chunks = escapedHtml.split(/\n{2,}/);
  return chunks
    .map((chunk) => {
      const trimmed = chunk.trim();
      if (!trimmed) return '';
      // If a chunk is a placeholder by itself, do not wrap in <p>.
      if (/^(THINK|CODE)\d+$/.test(trimmed)) return trimmed;
      return `<p>${trimmed.replace(/\n/g, '<br>')}</p>`;
    })
    .filter(Boolean)
    .join('');
}

/// Render a markdown-flavored string as sanitized HTML.
///   `opts.streaming` — domyslny stan otwarcia <think> bloku, gdy chat nie
///                      utrwala wyboru usera.
///   `opts.thinkKeyPrefix` — prefix do `data-think-key`; chat passuje msg.id,
///                           kazdy blok dostaje `${prefix}-${idx}`.
///   `opts.getThinkOpen(key)` — funkcja czytajaca persisted stan; gdy zwroci
///                              undefined, fallback do `streaming` defaultu.
export function renderMarkdown(text, opts = {}) {
  if (!text) return '';
  const streaming = opts.streaming === true;
  const keyPrefix = opts.thinkKeyPrefix || '';
  const getThinkOpen = typeof opts.getThinkOpen === 'function' ? opts.getThinkOpen : null;
  let thinkIdx = 0;

  // 1) Extract thinking blocks. Obslugujemy trzy warianty:
  //    a) standardowe pary <think>...</think> / <thinking>...</thinking>
  //    b) implicit-open: model zaczyna reasoning bez tagu otwierajacego
  //       (Gemini / niektore Qwen reasoning trybow), ale konczy </think>.
  //       Wtedy traktujemy caly prefix do pierwszego </think> jako thinking.
  //    c) streaming bez tagu zamykajacego (jeszcze nie doszedl) — zostawiamy
  //       jak jest, blok pojawi sie dopiero po dotarciu </think>.
  let preprocessed = text;
  const closingMatch = preprocessed.match(/<\/think(?:ing)?>/i);
  if (closingMatch) {
    const closeIdx = closingMatch.index;
    const before = preprocessed.slice(0, closeIdx);
    if (!/<think(?:ing)?>/i.test(before)) {
      preprocessed = '<think>' + preprocessed;
    }
  }
  const thinkRegex = /<think(?:ing)?>([\s\S]*?)<\/think(?:ing)?>/gi;
  const { text: t1, slots: thinkSlots } = withPlaceholders(preprocessed, 'THINK', {
    regex: thinkRegex,
    handler: (_m, inner) => {
      const key = keyPrefix ? `${keyPrefix}-${thinkIdx}` : '';
      thinkIdx += 1;
      const persisted = getThinkOpen && key ? getThinkOpen(key) : undefined;
      const isOpen = persisted === undefined ? streaming : persisted;
      return renderThinkingBlock(inner.trim(), isOpen, key);
    },
  });

  // 2) Extract code fences. ```lang[:filename]\n...\n```
  const fenceRegex = /^```([a-zA-Z0-9_+\-]*)(?::([^\n]+))?\n([\s\S]*?)```/gm;
  const { text: t2, slots: codeSlots } = withPlaceholders(t1, 'CODE', {
    regex: fenceRegex,
    handler: (_m, lang, filename, code) => renderCodeBlock(lang, filename, code),
  });

  // 3) Escape remaining text and apply inline + paragraph rules.
  const escaped = escapeHtml(t2);
  const inlined = applyInlineMarkdown(escaped);
  let html = paragraphize(inlined);

  // 4) Restore placeholders. Iterate from largest index down to avoid prefix overlaps.
  html = html.replace(/(THINK|CODE)(\d+)/g, (_m, kind, idxStr) => {
    const idx = Number(idxStr);
    return (kind === 'THINK' ? thinkSlots[idx] : codeSlots[idx]) || '';
  });
  return html;
}

/// Strip markup so the result is suitable for sidebar one-line previews.
export function extractPlainText(text) {
  if (!text) return '';
  let out = String(text);
  out = out.replace(/<think(?:ing)?>[\s\S]*?<\/think(?:ing)?>/gi, '');
  out = out.replace(/```[a-zA-Z0-9_+\-]*(?::[^\n]+)?\n[\s\S]*?```/g, '[code]');
  out = out.replace(/`([^`\n]+)`/g, '$1');
  out = out.replace(/\*\*([^*\n]+?)\*\*/g, '$1');
  out = out.replace(/\*([^*\n]+?)\*/g, '$1');
  return out.replace(/\s+/g, ' ').trim();
}
