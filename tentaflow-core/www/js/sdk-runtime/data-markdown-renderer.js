// =============================================================================
// Plik: sdk-runtime/data-markdown-renderer.js
// Opis: Renderer Markdown (0x0220) — chunk 3.3d-16. Safe constrained
// markdown parser: only features listed in allowed_features are rendered;
// all others pass through as escaped text. Zero innerHTML — every element
// is built via createElement + textContent.
//
// Supported features: heading, list, code_block, blockquote, table, link,
// image, emphasis, strong, code_inline.
//
// Security: all text goes through textContent. Links are validated against
// allowed protocols (http/https/mailto). Images same. link_target controls
// whether links open in new tab (blank_via_command) or same (self).
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/markdown.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  requireEnum, requireBool, requireU16, requireString,
  assertOnlyKnownFields,
} from './data-chart-shared.js';

export const MARKDOWN_TAG = 0x0220;
const MARKDOWN_FIELD_KEYS = new Set([0, 1, 2, 3]);
const LINK_TARGETS = new Set(['self', 'blank_via_command']);
const MARKDOWN_FEATURES = new Set([
  'heading', 'list', 'code_block', 'blockquote', 'table',
  'link', 'image', 'emphasis', 'strong', 'code_inline',
]);
const SAFE_LINK_RE = /^(?:https?:|mailto:)/i;

function renderMarkdown(component, ctx) {
  assertOnlyKnownFields(component.fields, MARKDOWN_FIELD_KEYS, 'Markdown');

  const contentBind = ctx.readField(component.fields, 0);
  if (contentBind == null) throw new TypeError('Markdown.content is required (BindRef)');
  assertBindRef(contentBind, 'Markdown.content');
  const featuresRaw = ctx.readField(component.fields, 1);
  const features = new Set();
  if (featuresRaw === null) throw new TypeError('Markdown.allowed_features: explicit null not allowed');
  if (featuresRaw !== undefined) {
    if (!Array.isArray(featuresRaw)) throw new TypeError('Markdown.allowed_features: expected Array<MarkdownFeature>');
    for (const f of featuresRaw) {
      if (!MARKDOWN_FEATURES.has(f)) throw new TypeError(`Markdown.allowed_features: unknown feature '${f}'`);
      features.add(f);
    }
  }
  const maxHeightRaw = ctx.readField(component.fields, 2);
  const maxHeightPx = maxHeightRaw != null ? requireU16(maxHeightRaw, 'Markdown.max_height_px') : null;
  if (maxHeightPx != null && maxHeightPx === 0) throw new TypeError('Markdown.max_height_px must be > 0');
  const linkTarget = requireEnum(ctx.readField(component.fields, 3), LINK_TARGETS, 'Markdown.link_target');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-markdown');
  if (maxHeightPx != null) {
    wrapper.style.maxHeight = `${maxHeightPx}px`;
    wrapper.style.overflow = 'auto';
  }

  const rebuild = () => {
    wrapper.replaceChildren();
    const raw = resolveBindRef(contentBind, ctx.store);
    if (raw == null || typeof raw !== 'string' || raw === '') return;
    const nodes = parseMarkdown(raw, features, linkTarget);
    for (const n of nodes) wrapper.appendChild(n);
  };
  rebuild();
  ctx.registerCleanup(subscribeBindRef(contentBind, ctx.store, rebuild));
  return wrapper;
}

// =============================================================================
// Block-level parser
// =============================================================================

/// Checks whether a line is a valid markdown table separator: every cell
/// between pipes must be `---` (with optional leading/trailing colons for
/// alignment). At least one cell required.
function isTableSeparatorLine(line) {
  if (typeof line !== 'string') return false;
  const trimmed = line.replace(/^\|/, '').replace(/\|$/, '');
  const cells = trimmed.split('|');
  if (cells.length === 0) return false;
  for (const c of cells) {
    if (!/^\s*:?-{3,}:?\s*$/.test(c)) return false;
  }
  return true;
}

function parseMarkdown(src, features, linkTarget) {
  const lines = src.split('\n');
  const blocks = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    // Fenced code block: ```
    if (features.has('code_block') && /^```/.test(line)) {
      const lang = line.slice(3).trim();
      const codeLines = [];
      i++;
      while (i < lines.length && !/^```/.test(lines[i])) {
        codeLines.push(lines[i]);
        i++;
      }
      i++; // skip closing ```
      const pre = document.createElement('pre');
      pre.classList.add('tf-markdown__code-block');
      const code = document.createElement('code');
      if (lang) code.setAttribute('data-language', lang);
      code.textContent = codeLines.join('\n');
      pre.appendChild(code);
      blocks.push(pre);
      continue;
    }

    // Heading: # .. ######
    if (features.has('heading') && /^#{1,6}\s/.test(line)) {
      const level = line.match(/^(#{1,6})\s/)[1].length;
      const text = line.slice(level + 1);
      const h = document.createElement(`h${level}`);
      h.classList.add('tf-markdown__heading');
      appendInline(h, text, features, linkTarget);
      blocks.push(h);
      i++;
      continue;
    }

    // Blockquote: >
    if (features.has('blockquote') && /^>\s?/.test(line)) {
      const bqLines = [];
      while (i < lines.length && /^>\s?/.test(lines[i])) {
        bqLines.push(lines[i].replace(/^>\s?/, ''));
        i++;
      }
      const bq = document.createElement('blockquote');
      bq.classList.add('tf-markdown__blockquote');
      const inner = parseMarkdown(bqLines.join('\n'), features, linkTarget);
      for (const n of inner) bq.appendChild(n);
      blocks.push(bq);
      continue;
    }

    // Unordered list: - or *
    if (features.has('list') && /^[\-\*]\s/.test(line)) {
      const ul = document.createElement('ul');
      ul.classList.add('tf-markdown__list');
      while (i < lines.length && /^[\-\*]\s/.test(lines[i])) {
        const li = document.createElement('li');
        appendInline(li, lines[i].replace(/^[\-\*]\s/, ''), features, linkTarget);
        ul.appendChild(li);
        i++;
      }
      blocks.push(ul);
      continue;
    }

    // Ordered list: 1. 2. etc
    if (features.has('list') && /^\d+\.\s/.test(line)) {
      const ol = document.createElement('ol');
      ol.classList.add('tf-markdown__list');
      while (i < lines.length && /^\d+\.\s/.test(lines[i])) {
        const li = document.createElement('li');
        appendInline(li, lines[i].replace(/^\d+\.\s/, ''), features, linkTarget);
        ol.appendChild(li);
        i++;
      }
      blocks.push(ol);
      continue;
    }

    // Table: | ... | ... | with valid separator on next line.
    if (features.has('table') && /^\|.+\|/.test(line) &&
        i + 1 < lines.length && isTableSeparatorLine(lines[i + 1])) {
      const tableLines = [];
      while (i < lines.length && /^\|.+\|/.test(lines[i])) {
        tableLines.push(lines[i]);
        i++;
      }
      const table = parseTable(tableLines, features, linkTarget);
      if (table) { blocks.push(table); continue; }
      // Fallback: if parseTable failed, rewind and let paragraph handle it.
      i -= tableLines.length;
    }

    // Empty line
    if (line.trim() === '') { i++; continue; }

    // Paragraph (default)
    const paraLines = [];
    while (i < lines.length && lines[i].trim() !== '' &&
           !(features.has('heading') && /^#{1,6}\s/.test(lines[i])) &&
           !(features.has('code_block') && /^```/.test(lines[i])) &&
           !(features.has('blockquote') && /^>\s?/.test(lines[i])) &&
           !(features.has('list') && /^[\-\*]\s/.test(lines[i])) &&
           !(features.has('list') && /^\d+\.\s/.test(lines[i])) &&
           !(features.has('table') && /^\|.+\|/.test(lines[i]) && i + 1 < lines.length && isTableSeparatorLine(lines[i + 1]))) {
      paraLines.push(lines[i]);
      i++;
    }
    const p = document.createElement('p');
    p.classList.add('tf-markdown__paragraph');
    appendInline(p, paraLines.join('\n'), features, linkTarget);
    blocks.push(p);
  }
  return blocks;
}

// =============================================================================
// Inline parser
// =============================================================================

function appendInline(parent, text, features, linkTarget) {
  // Process inline patterns left-to-right via regex scanning.
  // Priority: code_inline > image > link > strong > emphasis.
  const patterns = [];
  if (features.has('code_inline')) patterns.push({ re: /`([^`]+)`/g, kind: 'code' });
  if (features.has('image')) patterns.push({ re: /!\[([^\]]*)\]\(([^)]+)\)/g, kind: 'image' });
  if (features.has('link')) patterns.push({ re: /\[([^\]]+)\]\(([^)]+)\)/g, kind: 'link' });
  if (features.has('strong')) patterns.push({ re: /\*\*(.+?)\*\*/g, kind: 'strong' });
  if (features.has('emphasis')) patterns.push({ re: /(?<!\*)\*(?!\*)(.+?)(?<!\*)\*(?!\*)/g, kind: 'em' });

  if (patterns.length === 0) {
    parent.appendChild(document.createTextNode(text));
    return;
  }

  // Find all matches, sort by position, handle non-overlapping left-to-right.
  const matches = [];
  for (const p of patterns) {
    let m;
    while ((m = p.re.exec(text)) != null) {
      matches.push({ start: m.index, end: m.index + m[0].length, kind: p.kind, groups: m });
    }
  }
  matches.sort((a, b) => a.start - b.start || a.end - b.end);

  // Remove overlaps.
  const filtered = [];
  let lastEnd = 0;
  for (const m of matches) {
    if (m.start >= lastEnd) {
      filtered.push(m);
      lastEnd = m.end;
    }
  }

  let cursor = 0;
  for (const m of filtered) {
    if (m.start > cursor) {
      parent.appendChild(document.createTextNode(text.slice(cursor, m.start)));
    }
    if (m.kind === 'code') {
      const el = document.createElement('code');
      el.classList.add('tf-markdown__code-inline');
      el.textContent = m.groups[1];
      parent.appendChild(el);
    } else if (m.kind === 'strong') {
      const el = document.createElement('strong');
      appendInline(el, m.groups[1], features, linkTarget);
      parent.appendChild(el);
    } else if (m.kind === 'em') {
      const el = document.createElement('em');
      appendInline(el, m.groups[1], features, linkTarget);
      parent.appendChild(el);
    } else if (m.kind === 'link') {
      const href = m.groups[2].trim();
      if (SAFE_LINK_RE.test(href)) {
        const a = document.createElement('a');
        a.classList.add('tf-markdown__link');
        a.href = href;
        a.textContent = m.groups[1];
        if (linkTarget === 'blank_via_command') {
          a.target = '_blank';
          a.rel = 'noopener noreferrer';
        }
        parent.appendChild(a);
      } else {
        parent.appendChild(document.createTextNode(m.groups[1]));
      }
    } else if (m.kind === 'image') {
      const src = m.groups[2].trim();
      if (SAFE_LINK_RE.test(src)) {
        const img = document.createElement('img');
        img.classList.add('tf-markdown__image');
        img.alt = m.groups[1];
        img.src = src;
        parent.appendChild(img);
      } else {
        parent.appendChild(document.createTextNode(m.groups[1]));
      }
    }
    cursor = m.end;
  }
  if (cursor < text.length) {
    parent.appendChild(document.createTextNode(text.slice(cursor)));
  }
}

// =============================================================================
// Table parser
// =============================================================================

function parseTable(lines, features, linkTarget) {
  if (lines.length < 2) return null;
  const parseRow = (line) => line.replace(/^\|/, '').replace(/\|$/, '').split('|').map(c => c.trim());
  const headers = parseRow(lines[0]);
  if (!isTableSeparatorLine(lines[1])) return null;
  const table = document.createElement('table');
  table.classList.add('tf-markdown__table');
  const thead = document.createElement('thead');
  const headRow = document.createElement('tr');
  for (const h of headers) {
    const th = document.createElement('th');
    appendInline(th, h, features, linkTarget);
    headRow.appendChild(th);
  }
  thead.appendChild(headRow);
  table.appendChild(thead);
  const tbody = document.createElement('tbody');
  for (let r = 2; r < lines.length; r++) {
    const cells = parseRow(lines[r]);
    const tr = document.createElement('tr');
    for (let c = 0; c < headers.length; c++) {
      const td = document.createElement('td');
      appendInline(td, cells[c] || '', features, linkTarget);
      tr.appendChild(td);
    }
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  return table;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataMarkdownRenderer() {
  if (!lookupComponentRenderer(MARKDOWN_TAG)) registerComponentRenderer(MARKDOWN_TAG, renderMarkdown);
}
