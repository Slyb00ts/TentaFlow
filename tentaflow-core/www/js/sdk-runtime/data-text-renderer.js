// =============================================================================
// Plik: sdk-runtime/data-text-renderer.js
// Opis: Renderery komponentów tekstowych §4 Data Display — chunk 3.3d-1:
//   - Text       (0x0201) — single-string z TextStyle + tone + align + max_lines
//   - Heading    (0x0202) — semantic <h1>..<h6> z level + tone + align
//   - Paragraph  (0x0203) — multi-line z markdown subset (bold/italic/code/link)
//   - RichText   (0x0204) — markdown z allowed_blocks/marks
//   - MonoBlock  (0x0205) — preformatted text + word_wrap + copyable
//   - CodeBlock  (0x0206) — syntax-highlighted code (visual hint przez classę
//                            tf-codeblock--lang-X; pełne kolorowanie po stronie
//                            CSS theme'u lub późniejszego highlightera)
//
// **Bezpieczeństwo:** ŻADEN renderer NIE używa innerHTML z user input'em.
// Markdown parsing tworzy DOM tree przez document.createElement + textContent.
// Linki w Paragraph/RichText przechodzą walidator (https-only schemat).
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/text.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, formatValue } from './bind-resolver.js';

// =============================================================================
// Walidatory
// =============================================================================

const TEXT_STYLES = new Set([
  'display', 'title', 'h1', 'h2', 'h3', 'h4',
  'body_lg', 'body', 'body_strong', 'caption', 'caption_strong', 'overline',
  'code', 'mono', 'quote',
]);
const TEXT_ALIGNS = new Set(['start', 'center', 'end', 'justify']);
const TEXT_WRAPS = new Set(['wrap', 'nowrap', 'balance', 'pretty']);
const TONES = new Set(['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted']);
const MARKDOWN_MARKS = new Set(['bold', 'italic', 'code', 'link']);
const MARKDOWN_BLOCKS = new Set(['heading', 'list', 'code_block', 'blockquote', 'table']);
const VALUE_FORMAT_KINDS = new Set([
  'number', 'currency', 'percent', 'bytes', 'duration',
  'date', 'time', 'datetime', 'relative', 'plain',
]);
// Safe link schemes — https only per spec security model (NavigateExternal).
const SAFE_LINK_RE = /^https:\/\/[^\s<>"']+$/;
// Valid language ident dla CodeBlock — [a-z0-9_-]+ length 1..=32.
const LANGUAGE_RE = /^[a-z0-9_-]{1,32}$/;

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(`${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`);
  }
  return v;
}
function requireBool(v, ctx) {
  if (typeof v !== 'boolean') throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
  return v;
}
function requireU8(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFn) throw new TypeError(`${ctx}: expected u8, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) throw new TypeError(`${ctx}: expected u8, got ${v}`);
  return v;
}
function requireU16(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFn) throw new TypeError(`${ctx}: expected u16, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFF) throw new TypeError(`${ctx}: expected u16, got ${v}`);
  return v;
}
function requireU32(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFFFFFn) throw new TypeError(`${ctx}: expected u32, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFFFFFF) throw new TypeError(`${ctx}: expected u32, got ${v}`);
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}
function assertValueFormat(fmt, ctx, locale) {
  if (fmt == null) return;
  if (typeof fmt !== 'object' || Array.isArray(fmt)) {
    throw new TypeError(`${ctx}: ValueFormat must be object`);
  }
  if (typeof fmt.kind !== 'string' || !VALUE_FORMAT_KINDS.has(fmt.kind)) {
    throw new TypeError(`${ctx}: ValueFormat.kind invalid: ${fmt.kind}`);
  }
  // Eager probe — wywołaj formatValue z sample wartością odpowiednią dla
  // wariantu. bind-resolver waliduje variant-specific fields (decimals,
  // currency.code, bytes.base etc.) dopiero przy format-call; bez probe'a
  // niepoprawny `{ kind: 'currency' }` bez `code` przeszedłby przez render
  // i silentnie fallback'ował przy każdej apply().
  const probe = (fmt.kind === 'date' || fmt.kind === 'time' || fmt.kind === 'datetime' || fmt.kind === 'relative')
    ? 0  // unix-millis 1970-01-01
    : 0;  // 0 jako liczba dla numeric variants
  try {
    formatValue(probe, fmt, locale);
  } catch (err) {
    throw new TypeError(`${ctx}: invalid ValueFormat — ${err && err.message ? err.message : err}`);
  }
}

/// Reactive textContent. Gdy ValueFormat set, formatuje wartość przez Intl.
function applyReactiveText(element, bindRef, ctx, valueFormat) {
  const apply = () => {
    const raw = resolveBindRef(bindRef, ctx.store);
    if (raw == null) { element.textContent = ''; return; }
    if (valueFormat != null) {
      try {
        element.textContent = formatValue(raw, valueFormat, ctx.locale);
      } catch {
        element.textContent = String(raw);
      }
    } else {
      element.textContent = String(raw);
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

// =============================================================================
// Markdown subset parsing (SAFE — DOM tree construction)
// =============================================================================

/// Parser inline marks: **bold**, _italic_, `code`, [text](https://url).
/// Tworzy listę {kind, value, href?} segmentów dla danego allowed_marks setu.
/// Nieparsowalne sekwencje pozostają jako plain text (escaped przez
/// textContent w renderer'ze).
function parseInlineMarks(text, allowedMarks) {
  const allowBold = allowedMarks.has('bold');
  const allowItalic = allowedMarks.has('italic');
  const allowCode = allowedMarks.has('code');
  const allowLink = allowedMarks.has('link');
  const out = [];
  let buf = '';
  let i = 0;
  const flush = () => {
    if (buf.length > 0) { out.push({ kind: 'text', value: buf }); buf = ''; }
  };
  while (i < text.length) {
    const ch = text[i];
    // **bold**
    if (allowBold && ch === '*' && text[i + 1] === '*') {
      const end = text.indexOf('**', i + 2);
      if (end > i + 2) {
        flush();
        out.push({ kind: 'bold', value: text.slice(i + 2, end) });
        i = end + 2;
        continue;
      }
    }
    // _italic_ — wymaga word-boundary po stronie open (nie wpośrodku snake_case).
    if (allowItalic && ch === '_') {
      const before = i === 0 ? '' : text[i - 1];
      if (!/[A-Za-z0-9_]/.test(before)) {
        const end = text.indexOf('_', i + 1);
        if (end > i + 1) {
          const after = text[end + 1];
          if (after === undefined || !/[A-Za-z0-9_]/.test(after)) {
            flush();
            out.push({ kind: 'italic', value: text.slice(i + 1, end) });
            i = end + 1;
            continue;
          }
        }
      }
    }
    // `code`
    if (allowCode && ch === '`') {
      const end = text.indexOf('`', i + 1);
      if (end > i + 1) {
        flush();
        out.push({ kind: 'code', value: text.slice(i + 1, end) });
        i = end + 1;
        continue;
      }
    }
    // [text](url)
    if (allowLink && ch === '[') {
      const closeBracket = text.indexOf(']', i + 1);
      if (closeBracket > i + 1 && text[closeBracket + 1] === '(') {
        const closeParen = text.indexOf(')', closeBracket + 2);
        if (closeParen > closeBracket + 2) {
          const label = text.slice(i + 1, closeBracket);
          const url = text.slice(closeBracket + 2, closeParen);
          if (SAFE_LINK_RE.test(url)) {
            flush();
            out.push({ kind: 'link', value: label, href: url });
            i = closeParen + 1;
            continue;
          }
        }
      }
    }
    buf += ch;
    i++;
  }
  flush();
  return out;
}

/// Renderuje listę inline-mark segmentów do DocumentFragment. SAFE —
/// wszystkie wartości przez textContent.
function renderInlineMarks(segments) {
  const frag = document.createDocumentFragment();
  for (const seg of segments) {
    let node;
    switch (seg.kind) {
      case 'bold':
        node = document.createElement('strong');
        node.textContent = seg.value;
        break;
      case 'italic':
        node = document.createElement('em');
        node.textContent = seg.value;
        break;
      case 'code':
        node = document.createElement('code');
        node.textContent = seg.value;
        break;
      case 'link':
        node = document.createElement('a');
        node.setAttribute('href', seg.href);
        node.setAttribute('rel', 'noopener noreferrer');
        node.setAttribute('target', '_blank');
        node.textContent = seg.value;
        break;
      case 'text':
      default:
        node = document.createTextNode(seg.value);
        break;
    }
    frag.appendChild(node);
  }
  return frag;
}

/// Renderuje RichText markdown z blokami. Bardzo prosty parser:
///   - heading: `# ` / `## ` / `### ` na początku linii
///   - blockquote: `> ` na początku linii
///   - list: `- ` / `* ` na początku linii (flat, brak nestingu)
///   - code_block: ```lang\n...\n```
///   - paragraf w pozostałych przypadkach
/// Wszystkie tekstowe wartości przez textContent (XSS-safe).
function renderRichTextBlocks(source, allowedBlocks, allowedMarks) {
  const root = document.createDocumentFragment();
  if (typeof source !== 'string') return root;
  const lines = source.split('\n');
  let i = 0;
  let currentList = null;
  const flushList = () => { currentList = null; };
  while (i < lines.length) {
    const line = lines[i];
    // ```lang code block
    if (allowedBlocks.has('code_block') && line.startsWith('```')) {
      flushList();
      const lang = line.slice(3).trim();
      const buf = [];
      i++;
      while (i < lines.length && !lines[i].startsWith('```')) {
        buf.push(lines[i]);
        i++;
      }
      const pre = document.createElement('pre');
      pre.classList.add('tf-richtext__code');
      if (lang.length > 0 && LANGUAGE_RE.test(lang)) pre.setAttribute('data-language', lang);
      pre.textContent = buf.join('\n');
      root.appendChild(pre);
      if (i < lines.length) i++;  // skip closing ```
      continue;
    }
    // # heading
    if (allowedBlocks.has('heading') && /^#{1,6}\s/.test(line)) {
      flushList();
      const m = line.match(/^(#{1,6})\s+(.*)$/);
      const level = m[1].length;
      const tag = `h${level}`;
      const h = document.createElement(tag);
      h.appendChild(renderInlineMarks(parseInlineMarks(m[2], allowedMarks)));
      root.appendChild(h);
      i++;
      continue;
    }
    // > blockquote
    if (allowedBlocks.has('blockquote') && line.startsWith('> ')) {
      flushList();
      const bq = document.createElement('blockquote');
      const p = document.createElement('p');
      p.appendChild(renderInlineMarks(parseInlineMarks(line.slice(2), allowedMarks)));
      bq.appendChild(p);
      root.appendChild(bq);
      i++;
      continue;
    }
    // - list
    if (allowedBlocks.has('list') && /^[-*]\s/.test(line)) {
      if (currentList == null) {
        currentList = document.createElement('ul');
        root.appendChild(currentList);
      }
      const li = document.createElement('li');
      li.appendChild(renderInlineMarks(parseInlineMarks(line.slice(2), allowedMarks)));
      currentList.appendChild(li);
      i++;
      continue;
    }
    flushList();
    // empty line skip
    if (line.trim() === '') { i++; continue; }
    // paragraph
    const p = document.createElement('p');
    p.appendChild(renderInlineMarks(parseInlineMarks(line, allowedMarks)));
    root.appendChild(p);
    i++;
  }
  return root;
}

// =============================================================================
// Text (0x0201)
// =============================================================================

export const TEXT_TAG = 0x0201;
const TEXT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);

function renderText(component, ctx) {
  assertOnlyKnownFields(component.fields, TEXT_FIELD_KEYS, 'Text');

  const content = ctx.readField(component.fields, 0);
  if (content == null) throw new TypeError('Text.content is required (BindRef)');
  const style = requireEnum(ctx.readField(component.fields, 1), TEXT_STYLES, 'Text.style');
  const toneRaw = ctx.readField(component.fields, 2);
  const tone = toneRaw == null ? null : requireEnum(toneRaw, TONES, 'Text.tone');
  const alignRaw = ctx.readField(component.fields, 3);
  const align = alignRaw == null ? null : requireEnum(alignRaw, TEXT_ALIGNS, 'Text.align');
  const wrapRaw = ctx.readField(component.fields, 4);
  const wrap = wrapRaw == null ? null : requireEnum(wrapRaw, TEXT_WRAPS, 'Text.wrap');
  const maxLinesRaw = ctx.readField(component.fields, 5);
  const maxLines = maxLinesRaw == null ? null : requireU8(maxLinesRaw, 'Text.max_lines');
  if (maxLines != null && maxLines === 0) {
    throw new TypeError('Text.max_lines must be > 0 if set');
  }
  const format = ctx.readField(component.fields, 6);
  assertValueFormat(format, 'Text.format', ctx.locale);
  // streaming: Option<BindRef> — while the bound flag is truthy the renderer
  // shows a semantic blinking caret (`.sdk-text--streaming`), so the addon
  // declares "this text is mid-stream" instead of animating its own cursor.
  const streaming = ctx.readField(component.fields, 7);

  const el = document.createElement('span');
  el.classList.add('tf-text');
  el.classList.add(`tf-text--style-${style}`);
  if (tone) el.classList.add(`tf-text--tone-${tone}`);
  if (align) el.classList.add(`tf-text--align-${align}`);
  if (wrap) el.classList.add(`tf-text--wrap-${wrap}`);
  if (maxLines != null) {
    el.classList.add('tf-text--clamp');
    el.style.setProperty('--tf-text-max-lines', String(maxLines));
  }
  applyReactiveText(el, content, ctx, format);

  if (streaming != null) {
    const applyStreaming = () => {
      const on = resolveBindRef(streaming, ctx.store) === true;
      el.classList.toggle('sdk-text--streaming', on);
    };
    applyStreaming();
    ctx.registerCleanup(subscribeBindRef(streaming, ctx.store, applyStreaming));
  }
  return el;
}

// =============================================================================
// Heading (0x0202)
// =============================================================================

export const HEADING_TAG = 0x0202;
const HEADING_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderHeading(component, ctx) {
  assertOnlyKnownFields(component.fields, HEADING_FIELD_KEYS, 'Heading');

  const content = ctx.readField(component.fields, 0);
  if (content == null) throw new TypeError('Heading.content is required');
  const level = requireU8(ctx.readField(component.fields, 1), 'Heading.level');
  if (level < 1 || level > 6) throw new TypeError('Heading.level must be 1..=6');
  const toneRaw = ctx.readField(component.fields, 2);
  const tone = toneRaw == null ? null : requireEnum(toneRaw, TONES, 'Heading.tone');
  const alignRaw = ctx.readField(component.fields, 3);
  const align = alignRaw == null ? null : requireEnum(alignRaw, TEXT_ALIGNS, 'Heading.align');

  const el = document.createElement(`h${level}`);
  el.classList.add('tf-heading');
  el.classList.add(`tf-heading--level-${level}`);
  if (tone) el.classList.add(`tf-heading--tone-${tone}`);
  if (align) el.classList.add(`tf-heading--align-${align}`);
  applyReactiveText(el, content, ctx, null);
  return el;
}

// =============================================================================
// Paragraph (0x0203)
// =============================================================================

export const PARAGRAPH_TAG = 0x0203;
const PARAGRAPH_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderParagraph(component, ctx) {
  assertOnlyKnownFields(component.fields, PARAGRAPH_FIELD_KEYS, 'Paragraph');

  const content = ctx.readField(component.fields, 0);
  if (content == null) throw new TypeError('Paragraph.content is required');
  // §4 0x0203 default: style = body.
  const styleRaw = ctx.readField(component.fields, 1);
  const style = styleRaw === undefined
    ? 'body'
    : requireEnum(styleRaw, TEXT_STYLES, 'Paragraph.style');
  const allowedMarksRaw = ctx.readField(component.fields, 2);
  const allowedMarksArr = allowedMarksRaw == null ? [] : (() => {
    if (!Array.isArray(allowedMarksRaw)) {
      throw new TypeError('Paragraph.allowed_marks: expected Array<MarkdownMark>');
    }
    return allowedMarksRaw.map((m, i) => requireEnum(m, MARKDOWN_MARKS, `Paragraph.allowed_marks[${i}]`));
  })();
  const allowedMarks = new Set(allowedMarksArr);
  const allowLinks = requireBool(ctx.readField(component.fields, 3), 'Paragraph.allow_links');
  // Jeśli allow_links=false, usuwamy 'link' z allowed_marks niezależnie od
  // tego co addon zadeklarował.
  if (!allowLinks) allowedMarks.delete('link');
  const maxLinesRaw = ctx.readField(component.fields, 4);
  const maxLines = maxLinesRaw == null ? null : requireU8(maxLinesRaw, 'Paragraph.max_lines');
  if (maxLines != null && maxLines === 0) {
    throw new TypeError('Paragraph.max_lines must be > 0 if set');
  }

  const el = document.createElement('p');
  el.classList.add('tf-paragraph');
  el.classList.add(`tf-paragraph--style-${style}`);
  if (maxLines != null) {
    el.classList.add('tf-paragraph--clamp');
    el.style.setProperty('--tf-text-max-lines', String(maxLines));
  }

  const apply = () => {
    const raw = resolveBindRef(content, ctx.store);
    const text = raw == null ? '' : String(raw);
    el.replaceChildren();
    if (allowedMarks.size > 0) {
      const segs = parseInlineMarks(text, allowedMarks);
      el.appendChild(renderInlineMarks(segs));
    } else {
      // Bez allowed_marks: plain textContent (escape XSS).
      el.textContent = text;
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(content, ctx.store, apply));
  return el;
}

// =============================================================================
// RichText (0x0204)
// =============================================================================

export const RICH_TEXT_TAG = 0x0204;
const RICH_TEXT_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderRichText(component, ctx) {
  assertOnlyKnownFields(component.fields, RICH_TEXT_FIELD_KEYS, 'RichText');

  const content = ctx.readField(component.fields, 0);
  if (content == null) throw new TypeError('RichText.content is required');
  const allowedBlocksRaw = ctx.readField(component.fields, 1);
  const allowedBlocksArr = allowedBlocksRaw == null ? [] : (() => {
    if (!Array.isArray(allowedBlocksRaw)) {
      throw new TypeError('RichText.allowed_blocks: expected Array<MarkdownBlock>');
    }
    return allowedBlocksRaw.map((b, i) => requireEnum(b, MARKDOWN_BLOCKS, `RichText.allowed_blocks[${i}]`));
  })();
  const allowedBlocks = new Set(allowedBlocksArr);
  const allowedMarksRaw = ctx.readField(component.fields, 2);
  const allowedMarksArr = allowedMarksRaw == null ? [] : (() => {
    if (!Array.isArray(allowedMarksRaw)) {
      throw new TypeError('RichText.allowed_marks: expected Array<MarkdownMark>');
    }
    return allowedMarksRaw.map((m, i) => requireEnum(m, MARKDOWN_MARKS, `RichText.allowed_marks[${i}]`));
  })();
  const allowedMarks = new Set(allowedMarksArr);
  const maxHeightRaw = ctx.readField(component.fields, 3);
  const maxHeightPx = maxHeightRaw == null ? null : requireU16(maxHeightRaw, 'RichText.max_height_px');
  if (maxHeightPx != null && maxHeightPx === 0) {
    throw new TypeError('RichText.max_height_px must be > 0 if set');
  }

  const el = document.createElement('div');
  el.classList.add('tf-richtext');
  if (maxHeightPx != null) {
    el.classList.add('tf-richtext--bounded');
    el.style.maxHeight = `${maxHeightPx}px`;
    el.style.overflowY = 'auto';
  }

  const apply = () => {
    const raw = resolveBindRef(content, ctx.store);
    const text = raw == null ? '' : String(raw);
    el.replaceChildren();
    el.appendChild(renderRichTextBlocks(text, allowedBlocks, allowedMarks));
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(content, ctx.store, apply));
  return el;
}

// =============================================================================
// MonoBlock (0x0205)
// =============================================================================

export const MONO_BLOCK_TAG = 0x0205;
const MONO_BLOCK_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderMonoBlock(component, ctx) {
  assertOnlyKnownFields(component.fields, MONO_BLOCK_FIELD_KEYS, 'MonoBlock');

  const content = ctx.readField(component.fields, 0);
  if (content == null) throw new TypeError('MonoBlock.content is required');
  const maxHeightRaw = ctx.readField(component.fields, 1);
  const maxHeightPx = maxHeightRaw == null ? null : requireU16(maxHeightRaw, 'MonoBlock.max_height_px');
  if (maxHeightPx != null && maxHeightPx === 0) {
    throw new TypeError('MonoBlock.max_height_px must be > 0 if set');
  }
  const wordWrap = requireBool(ctx.readField(component.fields, 2), 'MonoBlock.word_wrap');
  const copyable = requireBool(ctx.readField(component.fields, 3), 'MonoBlock.copyable');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-monoblock');
  if (wordWrap) wrapper.classList.add('tf-monoblock--wrap');

  const pre = document.createElement('pre');
  pre.classList.add('tf-monoblock__content');
  if (maxHeightPx != null) {
    pre.style.maxHeight = `${maxHeightPx}px`;
    pre.style.overflow = 'auto';
  }
  applyReactiveText(pre, content, ctx, null);
  wrapper.appendChild(pre);

  if (copyable) {
    const btn = document.createElement('button');
    btn.setAttribute('type', 'button');
    btn.classList.add('tf-monoblock__copy');
    btn.setAttribute('aria-label', 'Copy to clipboard');
    btn.textContent = 'Copy';
    const onClick = async (e) => {
      e.preventDefault();
      try {
        if (globalThis.navigator && globalThis.navigator.clipboard) {
          await globalThis.navigator.clipboard.writeText(pre.textContent);
          btn.textContent = 'Copied';
          setTimeout(() => { btn.textContent = 'Copy'; }, 1500);
        }
      } catch {}
    };
    btn.addEventListener('click', onClick);
    ctx.registerCleanup(() => btn.removeEventListener('click', onClick));
    wrapper.appendChild(btn);
  }

  return wrapper;
}

// =============================================================================
// CodeBlock (0x0206)
// =============================================================================

export const CODE_BLOCK_TAG = 0x0206;
const CODE_BLOCK_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderCodeBlock(component, ctx) {
  assertOnlyKnownFields(component.fields, CODE_BLOCK_FIELD_KEYS, 'CodeBlock');

  const content = ctx.readField(component.fields, 0);
  if (content == null) throw new TypeError('CodeBlock.content is required');
  const language = requireString(ctx.readField(component.fields, 1), 'CodeBlock.language');
  if (!LANGUAGE_RE.test(language)) {
    throw new TypeError('CodeBlock.language must match [a-z0-9_-]+ length 1..=32');
  }
  const showLineNumbers = requireBool(ctx.readField(component.fields, 2), 'CodeBlock.show_line_numbers');
  const copyable = requireBool(ctx.readField(component.fields, 3), 'CodeBlock.copyable');
  const maxHeightRaw = ctx.readField(component.fields, 4);
  const maxHeightPx = maxHeightRaw == null ? null : requireU16(maxHeightRaw, 'CodeBlock.max_height_px');
  if (maxHeightPx != null && maxHeightPx === 0) {
    throw new TypeError('CodeBlock.max_height_px must be > 0 if set');
  }
  const highlightLinesRaw = ctx.readField(component.fields, 5);
  const highlightLines = highlightLinesRaw == null ? [] : (() => {
    if (!Array.isArray(highlightLinesRaw)) {
      throw new TypeError('CodeBlock.highlight_lines: expected Array<u32>');
    }
    return highlightLinesRaw.map((n, i) => requireU32(n, `CodeBlock.highlight_lines[${i}]`));
  })();
  const highlightSet = new Set(highlightLines);

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-codeblock');
  wrapper.classList.add(`tf-codeblock--lang-${language}`);
  if (showLineNumbers) wrapper.classList.add('tf-codeblock--lineno');
  wrapper.setAttribute('data-language', language);

  const pre = document.createElement('pre');
  pre.classList.add('tf-codeblock__content');
  if (maxHeightPx != null) {
    pre.style.maxHeight = `${maxHeightPx}px`;
    pre.style.overflow = 'auto';
  }

  const apply = () => {
    const raw = resolveBindRef(content, ctx.store);
    const text = raw == null ? '' : String(raw);
    pre.replaceChildren();
    if (showLineNumbers || highlightSet.size > 0) {
      const lines = text.split('\n');
      for (let i = 0; i < lines.length; i++) {
        const lineNo = i + 1;
        const lineEl = document.createElement('div');
        lineEl.classList.add('tf-codeblock__line');
        if (highlightSet.has(lineNo)) {
          lineEl.classList.add('tf-codeblock__line--highlighted');
        }
        if (showLineNumbers) {
          const gut = document.createElement('span');
          gut.classList.add('tf-codeblock__gutter');
          gut.setAttribute('aria-hidden', 'true');
          gut.textContent = String(lineNo);
          lineEl.appendChild(gut);
        }
        const code = document.createElement('code');
        code.classList.add('tf-codeblock__code');
        code.textContent = lines[i];
        lineEl.appendChild(code);
        pre.appendChild(lineEl);
      }
    } else {
      const code = document.createElement('code');
      code.classList.add('tf-codeblock__code');
      code.textContent = text;
      pre.appendChild(code);
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(content, ctx.store, apply));
  wrapper.appendChild(pre);

  if (copyable) {
    const btn = document.createElement('button');
    btn.setAttribute('type', 'button');
    btn.classList.add('tf-codeblock__copy');
    btn.setAttribute('aria-label', 'Copy code to clipboard');
    btn.textContent = 'Copy';
    const onClick = async (e) => {
      e.preventDefault();
      try {
        if (globalThis.navigator && globalThis.navigator.clipboard) {
          const raw = resolveBindRef(content, ctx.store);
          await globalThis.navigator.clipboard.writeText(raw == null ? '' : String(raw));
          btn.textContent = 'Copied';
          setTimeout(() => { btn.textContent = 'Copy'; }, 1500);
        }
      } catch {}
    };
    btn.addEventListener('click', onClick);
    ctx.registerCleanup(() => btn.removeEventListener('click', onClick));
    wrapper.appendChild(btn);
  }

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataTextRenderers() {
  if (!lookupComponentRenderer(TEXT_TAG)) registerComponentRenderer(TEXT_TAG, renderText);
  if (!lookupComponentRenderer(HEADING_TAG)) registerComponentRenderer(HEADING_TAG, renderHeading);
  if (!lookupComponentRenderer(PARAGRAPH_TAG)) registerComponentRenderer(PARAGRAPH_TAG, renderParagraph);
  if (!lookupComponentRenderer(RICH_TEXT_TAG)) registerComponentRenderer(RICH_TEXT_TAG, renderRichText);
  if (!lookupComponentRenderer(MONO_BLOCK_TAG)) registerComponentRenderer(MONO_BLOCK_TAG, renderMonoBlock);
  if (!lookupComponentRenderer(CODE_BLOCK_TAG)) registerComponentRenderer(CODE_BLOCK_TAG, renderCodeBlock);
}
