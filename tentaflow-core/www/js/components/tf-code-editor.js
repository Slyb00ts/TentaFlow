// =============================================================================
// File: tf-code-editor.js — dependency-free virtualized code editor
// Description: <tf-code-editor> — the dashboard's own code editor (no external
//   libraries, CSP-safe: no eval, user text rendered via textContent only).
//   Line-buffer document + windowed rendering (only visible rows hit the DOM,
//   smooth on 10k+ line files), per-language tokenizer with cached line-entry
//   states (multi-line strings/comments), indentation/brace folding, own
//   undo/redo stack with typing coalescing, find/replace bar (literal + regex),
//   bracket matching, auto-indent and auto-closing pairs.
//
//   Input model: a hidden <textarea> mirrors the current selection (CM5 style)
//   so native copy/cut/paste/IME work; all rendering (caret, selection,
//   highlights) is custom, which is what makes folding + virtualization exact.
//
//   Attributes: language (python|javascript|typescript|json|yaml|markdown|
//     gherkin|plain), readonly, wrap, tab-size, aria-label.
//   Properties: value, language, readOnly, wrap, tabSize, dirty (read-only),
//     labels (i18n dict — the component ships English fallbacks only).
//   Methods : getSelection() -> {from:{line,ch}, to:{line,ch}, text},
//     setSelection(from, to), replaceSelection(text), gotoLine(line /*1-based*/),
//     openSearch(withReplace), markClean(), foldAll(), unfoldAll(), focus().
//   Events  : "change" (debounced; detail {value, dirty}),
//     "selection-change" (detail {from, to, text}), "save" (detail {value}).
//
// Example:
//   const ed = document.createElement('tf-code-editor');
//   ed.setAttribute('language', 'python');
//   ed.value = 'def main():\n    pass\n';
//   ed.addEventListener('change', (e) => save(e.detail.value));
// =============================================================================

import { adoptControlsInto } from './shared-styles.js';
import './tf-button.js';
import './tf-input.js';

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const LINE_H = 20;              // px, must match the stylesheet
const PAD_X = 12;               // horizontal text padding inside the content area
const OVERSCAN = 10;            // extra rows rendered above/below the viewport
const MAX_MATCHES = 20000;      // search result cap
const BRACKET_SCAN_LIMIT = 20000; // chars scanned when matching a bracket
const FONT = `500 12px 'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;

const DEFAULT_LABELS = {
  editor: 'Code editor',
  find: 'Find',
  replace: 'Replace',
  match_case: 'Match case',
  regex: 'Regular expression',
  prev: 'Previous match',
  next: 'Next match',
  replace_one: 'Replace',
  replace_all: 'All',
  close: 'Close',
  matches: '{i}/{n}',
  no_matches: 'No results',
  bad_regex: 'Invalid regex',
  folded_lines: '{n} lines',
};

// Indent unit per language (spaces). tab-size attribute overrides.
const INDENT_UNITS = {
  python: 4, plain: 4, markdown: 2, gherkin: 2,
  javascript: 2, typescript: 2, json: 2, yaml: 2,
};

const PAIRS = { '(': ')', '[': ']', '{': '}' };
const CLOSERS = { ')': '(', ']': '[', '}': '{' };
const QUOTES = new Set(['"', "'", '`']);

const WORD_RE = /[\p{L}\p{N}_]/u;

// ---------------------------------------------------------------------------
// Tokenizers. tokenizeLine(lang, text, state) -> { toks: [start, end, cls, ...],
// state: <string state at end of line> }. state '' is the base state; other
// values encode an open multi-line construct. Token classes: kw str com num fn
// op dec prop. Untokenized gaps render as plain text.
// ---------------------------------------------------------------------------

const PY_KW = new Set(('False None True and as assert async await break class continue def del elif else except '
  + 'finally for from global if import in is lambda nonlocal not or pass raise return try while with yield '
  + 'match case self cls').split(' '));

const JS_KW = new Set(('abstract any as asserts async await boolean break case catch class const continue debugger '
  + 'declare default delete do else enum export extends false finally for from function get if implements import '
  + 'in infer instanceof interface is keyof let namespace never new null number object of override private '
  + 'protected public readonly return satisfies set static string super switch symbol this throw true try type '
  + 'typeof undefined unique unknown var void while with yield').split(' '));

const YAML_KW = new Set(['true', 'false', 'yes', 'no', 'on', 'off', 'null', '~',
  'True', 'False', 'Yes', 'No', 'On', 'Off', 'Null', 'TRUE', 'FALSE', 'NULL']);

// Gherkin: feature-level keywords need a trailing ':', step keywords a space.
const GHERKIN_FEATURE = ['Feature', 'Rule', 'Background', 'Scenario Outline', 'Scenario Template', 'Scenario',
  'Examples', 'Example', 'Funkcja', 'Właściwość', 'Potrzeba biznesowa', 'Aspekt', 'Zdolność', 'Założenia',
  'Szablon scenariusza', 'Scenariusz', 'Przykłady', 'Przykład'];
const GHERKIN_STEP = ['Given', 'When', 'Then', 'And', 'But', 'Zakładając, że', 'Zakładając', 'Mając', 'Gdy',
  'Kiedy', 'Jeśli', 'Jeżeli', 'Wtedy', 'Oraz', 'Ale', 'I'];

const NUM_RE = /^(?:0[xXoObB][0-9a-fA-F_]+|\d[\d_]*(?:\.[\d_]*)?(?:[eE][+-]?\d+)?[jJnN]?|\.\d[\d_]*(?:[eE][+-]?\d+)?)/;

function isIdentStart(ch) { return /[\p{L}_$]/u.test(ch); }
function readIdent(text, i) {
  let j = i + 1;
  while (j < text.length && /[\p{L}\p{N}_$]/u.test(text[j])) j++;
  return j;
}

// Scans a single-line quoted string starting at `i` (quote at text[i]).
// Returns the index just past the closing quote (or line end).
function scanQuoted(text, i, quote) {
  let j = i + 1;
  while (j < text.length) {
    if (text[j] === '\\') { j += 2; continue; }
    if (text[j] === quote) return j + 1;
    j++;
  }
  return j;
}

function tokPython(text, state, toks) {
  let i = 0;
  if (state.startsWith('py:')) {
    const quote = state.slice(3); // """ or '''
    const close = text.indexOf(quote);
    if (close === -1) { toks.push(0, text.length, 'str'); return state; }
    toks.push(0, close + 3, 'str');
    i = close + 3;
    state = '';
  }
  let atLineStart = true; // only whitespace seen so far (decorator detection)
  let prevWord = '';
  while (i < text.length) {
    const ch = text[i];
    if (ch === ' ' || ch === '\t') { i++; continue; }
    if (ch === '#') { toks.push(i, text.length, 'com'); break; }
    if (ch === '@' && atLineStart) {
      let j = i + 1;
      while (j < text.length && /[\p{L}\p{N}_.]/u.test(text[j])) j++;
      toks.push(i, j, 'dec');
      i = j; atLineStart = false; continue;
    }
    atLineStart = false;
    // String with optional prefix (r/b/u/f in any case, up to 3 letters).
    if (/[rbufRBUF]/.test(ch) || ch === '"' || ch === "'") {
      let p = i;
      while (p < text.length && p - i < 3 && /[rbufRBUF]/.test(text[p])) p++;
      if (p < text.length && (text[p] === '"' || text[p] === "'")) {
        const isF = /[fF]/.test(text.slice(i, p));
        const q = text[p];
        const triple = text.slice(p, p + 3) === q + q + q;
        if (triple) {
          const close = text.indexOf(q + q + q, p + 3);
          if (close === -1) { toks.push(i, text.length, 'str'); return 'py:' + q + q + q; }
          toks.push(i, close + 3, 'str');
          i = close + 3; prevWord = ''; continue;
        }
        if (isF) {
          // f-string: interpolation braces break out of string colouring.
          let j = p + 1, runStart = i;
          while (j < text.length) {
            const c = text[j];
            if (c === '\\') { j += 2; continue; }
            if (c === q) { j++; break; }
            if (c === '{' && text[j + 1] === '{') { j += 2; continue; }
            if (c === '}' && text[j + 1] === '}') { j += 2; continue; }
            if (c === '{') {
              if (j > runStart) toks.push(runStart, j, 'str');
              toks.push(j, j + 1, 'op');
              let depth = 1, k = j + 1;
              while (k < text.length && depth > 0) {
                if (text[k] === '{') depth++;
                else if (text[k] === '}') depth--;
                if (depth > 0) k++;
              }
              if (k < text.length) { toks.push(k, k + 1, 'op'); j = k + 1; }
              else { j = k; }
              runStart = j;
              continue;
            }
            j++;
          }
          if (j > runStart) toks.push(runStart, j, 'str');
          i = j; prevWord = ''; continue;
        }
        const end = scanQuoted(text, p, q);
        toks.push(i, end, 'str');
        i = end; prevWord = ''; continue;
      }
    }
    if (isIdentStart(ch)) {
      const j = readIdent(text, i);
      const word = text.slice(i, j);
      if (PY_KW.has(word)) toks.push(i, j, 'kw');
      else if (prevWord === 'def' || prevWord === 'class' || text[j] === '(') toks.push(i, j, 'fn');
      prevWord = word;
      i = j; continue;
    }
    if (/\d/.test(ch) || (ch === '.' && /\d/.test(text[i + 1] || ''))) {
      const m = NUM_RE.exec(text.slice(i));
      if (m) { toks.push(i, i + m[0].length, 'num'); i += m[0].length; prevWord = ''; continue; }
    }
    if (/[+\-*/%=<>!&|^~?:;,.()[\]{}@]/.test(ch)) { toks.push(i, i + 1, 'op'); i++; prevWord = ''; continue; }
    i++; prevWord = '';
  }
  return '';
}

function tokJsLike(text, state, toks) {
  let i = 0;
  if (state === 'js:c') {
    const close = text.indexOf('*/');
    if (close === -1) { toks.push(0, text.length, 'com'); return state; }
    toks.push(0, close + 2, 'com');
    i = close + 2;
    state = '';
  } else if (state === 'js:`') {
    const res = scanTemplate(text, 0, toks);
    if (res.open) return 'js:`';
    i = res.end;
    state = '';
  }
  let prevWord = '';
  while (i < text.length) {
    const ch = text[i];
    if (ch === ' ' || ch === '\t') { i++; continue; }
    if (ch === '/' && text[i + 1] === '/') { toks.push(i, text.length, 'com'); break; }
    if (ch === '/' && text[i + 1] === '*') {
      const close = text.indexOf('*/', i + 2);
      if (close === -1) { toks.push(i, text.length, 'com'); return 'js:c'; }
      toks.push(i, close + 2, 'com');
      i = close + 2; continue;
    }
    if (ch === '"' || ch === "'") {
      const end = scanQuoted(text, i, ch);
      toks.push(i, end, 'str');
      i = end; prevWord = ''; continue;
    }
    if (ch === '`') {
      toks.push(i, i + 1, 'str');
      const res = scanTemplate(text, i + 1, toks);
      if (res.open) return 'js:`';
      i = res.end; prevWord = ''; continue;
    }
    if (ch === '@' && isIdentStart(text[i + 1] || '')) {
      const j = readIdent(text, i + 1);
      toks.push(i, j, 'dec');
      i = j; continue;
    }
    if (isIdentStart(ch)) {
      const j = readIdent(text, i);
      const word = text.slice(i, j);
      if (JS_KW.has(word)) toks.push(i, j, 'kw');
      else if (prevWord === 'function' || prevWord === 'class' || prevWord === 'new' || text[j] === '(') toks.push(i, j, 'fn');
      prevWord = word;
      i = j; continue;
    }
    if (/\d/.test(ch) || (ch === '.' && /\d/.test(text[i + 1] || ''))) {
      const m = NUM_RE.exec(text.slice(i));
      if (m) { toks.push(i, i + m[0].length, 'num'); i += m[0].length; prevWord = ''; continue; }
    }
    if (/[+\-*/%=<>!&|^~?:;,.()[\]{}]/.test(ch)) { toks.push(i, i + 1, 'op'); i++; prevWord = ''; continue; }
    i++; prevWord = '';
  }
  return '';
}

// Template-literal body scanner. Emits str tokens; ${…} interpolations render
// plain with op braces. Interpolations are treated as single-line (an unclosed
// one keeps the template state — an accepted approximation).
function scanTemplate(text, i, toks) {
  let runStart = i, j = i;
  while (j < text.length) {
    const c = text[j];
    if (c === '\\') { j += 2; continue; }
    if (c === '`') {
      toks.push(runStart, j + 1, 'str');
      return { end: j + 1, open: false };
    }
    if (c === '$' && text[j + 1] === '{') {
      if (j > runStart) toks.push(runStart, j, 'str');
      toks.push(j, j + 2, 'op');
      let depth = 1, k = j + 2;
      while (k < text.length && depth > 0) {
        if (text[k] === '{') depth++;
        else if (text[k] === '}') depth--;
        if (depth > 0) k++;
      }
      if (k < text.length) { toks.push(k, k + 1, 'op'); j = k + 1; }
      else { j = k; }
      runStart = j;
      continue;
    }
    j++;
  }
  if (j > runStart) toks.push(runStart, j, 'str');
  return { end: j, open: true };
}

function tokJson(text, state, toks) {
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (ch === '"') {
      const end = scanQuoted(text, i, '"');
      let k = end;
      while (k < text.length && (text[k] === ' ' || text[k] === '\t')) k++;
      toks.push(i, end, text[k] === ':' ? 'prop' : 'str');
      i = end; continue;
    }
    if (/[-\d]/.test(ch)) {
      const m = /^-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(text.slice(i));
      if (m) { toks.push(i, i + m[0].length, 'num'); i += m[0].length; continue; }
    }
    if (isIdentStart(ch)) {
      const j = readIdent(text, i);
      const word = text.slice(i, j);
      if (word === 'true' || word === 'false' || word === 'null') toks.push(i, j, 'kw');
      i = j; continue;
    }
    if (/[{}[\]:,]/.test(ch)) { toks.push(i, i + 1, 'op'); i++; continue; }
    i++;
  }
  return '';
}

function tokYaml(text, state, toks) {
  if (state.startsWith('y:')) {
    const keyIndent = parseInt(state.slice(2), 10);
    if (/^\s*$/.test(text)) return state;
    const indent = text.length - text.trimStart().length;
    if (indent > keyIndent) { toks.push(0, text.length, 'str'); return state; }
    // fall through: the block scalar ended, tokenize normally
  }
  let i = 0;
  const indent = text.length - text.trimStart().length;
  i = indent;
  while (i < text.length && text[i] === '-' && (text[i + 1] === ' ' || i + 1 === text.length)) {
    toks.push(i, i + 1, 'op');
    i += 2;
  }
  // key: — up to an unquoted colon followed by space/EOL
  const keyM = /^([^:#'"\n]+?):(\s|$)/.exec(text.slice(i));
  if (keyM) {
    toks.push(i, i + keyM[1].length, 'prop');
    toks.push(i + keyM[1].length, i + keyM[1].length + 1, 'op');
    i += keyM[1].length + 1;
  }
  while (i < text.length) {
    const ch = text[i];
    if (ch === ' ' || ch === '\t') { i++; continue; }
    if (ch === '#') { toks.push(i, text.length, 'com'); break; }
    if (ch === '"' || ch === "'") {
      const end = scanQuoted(text, i, ch);
      toks.push(i, end, 'str');
      i = end; continue;
    }
    if (ch === '&' || ch === '*') {
      const j = readIdent(text, i);
      toks.push(i, j, 'dec');
      i = j; continue;
    }
    if ((ch === '|' || ch === '>') && /^\s*$/.test(text.slice(i + 1).replace(/[+-]?\d*/, ''))) {
      toks.push(i, text.length, 'op');
      return 'y:' + indent;
    }
    if (/\d/.test(ch) || (ch === '-' && /\d/.test(text[i + 1] || ''))) {
      const m = /^-?\d[\d_]*(?:\.[\d_]*)?(?:[eE][+-]?\d+)?/.exec(text.slice(i));
      if (m) { toks.push(i, i + m[0].length, 'num'); i += m[0].length; continue; }
    }
    if (isIdentStart(ch)) {
      const j = readIdent(text, i);
      if (YAML_KW.has(text.slice(i, j))) toks.push(i, j, 'kw');
      i = j; continue;
    }
    if (ch === '~') { toks.push(i, i + 1, 'kw'); i++; continue; }
    if (/[{}[\]:,]/.test(ch)) { toks.push(i, i + 1, 'op'); i++; continue; }
    i++;
  }
  return '';
}

function tokMarkdown(text, state, toks) {
  if (state === 'md:f') {
    if (/^\s*(```|~~~)/.test(text)) { toks.push(0, text.length, 'kw'); return ''; }
    return state; // fence body renders plain
  }
  if (/^\s*(```|~~~)/.test(text)) { toks.push(0, text.length, 'kw'); return 'md:f'; }
  if (/^#{1,6}\s/.test(text)) { toks.push(0, text.length, 'kw'); return ''; }
  if (/^\s*>/.test(text)) { toks.push(text.indexOf('>'), text.indexOf('>') + 1, 'op'); }
  const listM = /^(\s*)([-*+]|\d+\.)\s/.exec(text);
  if (listM) toks.push(listM[1].length, listM[1].length + listM[2].length, 'op');
  // inline code, bold/italic markers, links
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (ch === '`') {
      const close = text.indexOf('`', i + 1);
      if (close === -1) break;
      toks.push(i, close + 1, 'str');
      i = close + 1; continue;
    }
    if (ch === '*' || ch === '_') { toks.push(i, i + 1, 'op'); i++; continue; }
    if (ch === '[') {
      const closeB = text.indexOf(']', i);
      if (closeB !== -1 && text[closeB + 1] === '(') {
        const closeP = text.indexOf(')', closeB + 2);
        if (closeP !== -1) {
          toks.push(i, closeB + 1, 'fn');
          toks.push(closeB + 1, closeP + 1, 'str');
          i = closeP + 1; continue;
        }
      }
    }
    i++;
  }
  return '';
}

function tokGherkin(text, state, toks) {
  if (state.startsWith('gh:')) {
    const fence = state.slice(3);
    const idx = text.indexOf(fence);
    if (idx === -1) { toks.push(0, text.length, 'str'); return state; }
    toks.push(0, idx + fence.length, 'str');
    return '';
  }
  const trimmed = text.trimStart();
  const indent = text.length - trimmed.length;
  if (trimmed.startsWith('#')) { toks.push(indent, text.length, 'com'); return ''; }
  if (trimmed.startsWith('@')) {
    // tag line: every @tag token
    let i = indent;
    while (i < text.length) {
      if (text[i] === '@') {
        let j = i + 1;
        while (j < text.length && !/\s/.test(text[j])) j++;
        toks.push(i, j, 'dec');
        i = j;
      } else i++;
    }
    return '';
  }
  if (trimmed.startsWith('"""') || trimmed.startsWith('```')) {
    const fence = trimmed.slice(0, 3);
    toks.push(indent, text.length, 'str');
    return 'gh:' + fence;
  }
  if (trimmed.startsWith('|')) {
    for (let i = indent; i < text.length; i++) if (text[i] === '|') toks.push(i, i + 1, 'op');
  } else {
    let matched = 0;
    for (const kw of GHERKIN_FEATURE) {
      if (trimmed.startsWith(kw + ':')) { toks.push(indent, indent + kw.length + 1, 'kw'); matched = kw.length + 1; break; }
    }
    if (!matched) {
      for (const kw of GHERKIN_STEP) {
        if (trimmed.startsWith(kw + ' ')) { toks.push(indent, indent + kw.length, 'kw'); matched = kw.length; break; }
      }
    }
  }
  // strings, <placeholders>, numbers in the step body
  let i = indent;
  while (i < text.length) {
    const ch = text[i];
    if (ch === '"') {
      const end = scanQuoted(text, i, '"');
      toks.push(i, end, 'str');
      i = end; continue;
    }
    if (ch === '<') {
      const close = text.indexOf('>', i + 1);
      if (close !== -1 && close - i < 80) { toks.push(i, close + 1, 'prop'); i = close + 1; continue; }
    }
    if (/\d/.test(ch) && !WORD_RE.test(text[i - 1] || ' ')) {
      const m = /^\d[\d_]*(?:[.,]\d+)?/.exec(text.slice(i));
      if (m) { toks.push(i, i + m[0].length, 'num'); i += m[0].length; continue; }
    }
    i++;
  }
  return '';
}

function tokenizeLine(lang, text, state) {
  const toks = [];
  let end = '';
  switch (lang) {
    case 'python': end = tokPython(text, state, toks); break;
    case 'javascript':
    case 'typescript': end = tokJsLike(text, state, toks); break;
    case 'json': end = tokJson(text, state, toks); break;
    case 'yaml': end = tokYaml(text, state, toks); break;
    case 'markdown': end = tokMarkdown(text, state, toks); break;
    case 'gherkin': end = tokGherkin(text, state, toks); break;
    default: break;
  }
  return { toks, state: end };
}

// ---------------------------------------------------------------------------
// Folding
// ---------------------------------------------------------------------------

function lineIndent(text, tabSize) {
  let w = 0;
  for (let i = 0; i < text.length; i++) {
    if (text[i] === ' ') w++;
    else if (text[i] === '\t') w += tabSize - (w % tabSize);
    else return w;
  }
  return -1; // blank line
}

// Indentation folding: a line folds up to the last consecutive deeper line.
function computeIndentFolds(lines, tabSize) {
  const folds = new Map();
  const stack = []; // {line, indent}
  let prevNonBlank = -1;
  for (let i = 0; i < lines.length; i++) {
    const d = lineIndent(lines[i], tabSize);
    if (d === -1) continue;
    while (stack.length && stack[stack.length - 1].indent >= d) {
      const e = stack.pop();
      if (prevNonBlank > e.line) folds.set(e.line, prevNonBlank);
    }
    stack.push({ line: i, indent: d });
    prevNonBlank = i;
  }
  while (stack.length) {
    const e = stack.pop();
    if (prevNonBlank > e.line) folds.set(e.line, prevNonBlank);
  }
  return folds;
}

// Brace folding for js/ts/json with a minimal cross-line string/comment scanner.
function computeBraceFolds(lines) {
  const folds = new Map();
  const stack = [];
  let mode = ''; // '', 'c' (block comment), '`' (template)
  for (let i = 0; i < lines.length; i++) {
    const text = lines[i];
    let j = 0;
    while (j < text.length) {
      const ch = text[j];
      if (mode === 'c') {
        const close = text.indexOf('*/', j);
        if (close === -1) { j = text.length; break; }
        j = close + 2; mode = ''; continue;
      }
      if (mode === '`') {
        if (ch === '\\') { j += 2; continue; }
        if (ch === '`') { mode = ''; }
        j++; continue;
      }
      if (ch === '\\') { j += 2; continue; }
      if (ch === '/' && text[j + 1] === '/') { j = text.length; break; }
      if (ch === '/' && text[j + 1] === '*') { mode = 'c'; j += 2; continue; }
      if (ch === '"' || ch === "'") { j = scanQuoted(text, j, ch); continue; }
      if (ch === '`') { mode = '`'; j++; continue; }
      if (ch === '{') { stack.push(i); j++; continue; }
      if (ch === '}') {
        const open = stack.pop();
        if (open !== undefined && i > open && !folds.has(open)) folds.set(open, i);
        j++; continue;
      }
      j++;
    }
  }
  return folds;
}

// ---------------------------------------------------------------------------
// Position helpers
// ---------------------------------------------------------------------------

function cmpPos(a, b) { return a.line - b.line || a.ch - b.ch; }
function posEq(a, b) { return a.line === b.line && a.ch === b.ch; }
function copyPos(p) { return { line: p.line, ch: p.ch }; }

// End position of `text` inserted at `from`.
function advancePos(from, text) {
  const nl = text.lastIndexOf('\n');
  if (nl === -1) return { line: from.line, ch: from.ch + text.length };
  const lines = text.split('\n');
  return { line: from.line + lines.length - 1, ch: lines[lines.length - 1].length };
}

function fmt(template, vars) {
  return String(template).replace(/\{(\w+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m));
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

const STYLE = `
:host { display: block; height: 100%; min-height: 160px; }
* { box-sizing: border-box; }
.editor {
  position: relative; height: 100%; display: flex; flex-direction: column;
  background: var(--bg-input); border: 1px solid var(--border);
  border-radius: var(--radius-sm); overflow: hidden;
  font: ${FONT};
}
.editor.focused { border-color: var(--accent-1); box-shadow: 0 0 0 3px var(--accent-glow); }
:host([readonly]) .editor.focused { box-shadow: none; }
.scroller { position: relative; flex: 1; overflow: auto; overscroll-behavior: contain; }
.canvas { display: flex; align-items: flex-start; min-width: 100%; position: relative; }
.gutter {
  position: sticky; left: 0; z-index: 3; flex: none;
  background: var(--bg-2); border-right: 1px solid var(--border);
  color: var(--text-3); user-select: none;
}
.gutter-row {
  position: absolute; left: 0; right: 0; height: ${LINE_H}px; line-height: ${LINE_H}px;
  display: flex; align-items: center; font-size: 11px;
}
.g-num { flex: 1; text-align: right; font-variant-numeric: tabular-nums; }
.g-num.active { color: var(--text); }
.g-fold {
  width: 14px; text-align: center; cursor: pointer; font-size: 9px;
  color: var(--text-3); opacity: 0; transition: opacity .12s;
}
.gutter:hover .g-fold.can { opacity: 1; }
.g-fold.folded { opacity: 1; color: var(--accent-2); }
.g-fold.can:hover { color: var(--text); }
.content { position: relative; flex: none; cursor: text; }
.g-num { padding-right: 2px; }
.row {
  position: absolute; left: 0; height: ${LINE_H}px; line-height: ${LINE_H}px;
  white-space: pre; min-width: 100%;
}
.row-bg { position: absolute; inset: 0; pointer-events: none; }
.row-text { position: relative; padding-left: ${PAD_X}px; color: var(--text); }
.rect { position: absolute; top: 0; height: 100%; }
.rect.sel { background: color-mix(in srgb, var(--accent-1) 28%, transparent); }
.rect.cur-line { left: 0 !important; width: 100% !important; background: color-mix(in srgb, var(--text) 5%, transparent); }
.rect.match { background: color-mix(in srgb, var(--warning) 30%, transparent); border-radius: 2px; }
.rect.match-active { background: color-mix(in srgb, var(--warning) 55%, transparent); outline: 1px solid var(--warning); border-radius: 2px; }
.rect.bracket { outline: 1px solid var(--accent-1); border-radius: 2px; background: var(--accent-glow); }
.tk-kw { color: var(--accent-2); }
.tk-str { color: var(--success); }
.tk-com { color: var(--text-3); font-style: italic; }
.tk-num { color: var(--warning); }
.tk-fn { color: var(--info); }
.tk-op { color: var(--text-2); }
.tk-dec { color: var(--info); }
.tk-prop { color: var(--info); }
.fold-pill {
  display: inline-block; margin-left: 8px; padding: 0 7px; font-size: 10px;
  line-height: 15px; vertical-align: 2px; cursor: pointer; user-select: none;
  color: var(--text-3); background: var(--bg-3);
  border: 1px solid var(--border-hover); border-radius: 8px;
}
.fold-pill:hover { color: var(--text); border-color: var(--accent-1); }
.caret {
  position: absolute; width: 2px; height: ${LINE_H - 2}px; margin-top: 1px;
  background: var(--text); z-index: 2; pointer-events: none;
  animation: tf-caret-blink 1.1s steps(1) infinite;
}
.editor:not(.focused) .caret { display: none; }
@keyframes tf-caret-blink { 0%, 55% { opacity: 1; } 56%, 100% { opacity: 0; } }
.hidden-input {
  position: absolute; width: 2px; height: ${LINE_H}px; padding: 0; border: 0;
  margin: 0; outline: none; resize: none; overflow: hidden;
  background: transparent; color: transparent; caret-color: transparent;
  font: inherit; z-index: 1; pointer-events: none;
}
.findbar {
  position: absolute; top: 6px; right: 16px; z-index: 6;
  display: flex; gap: 6px; align-items: center; flex-wrap: wrap;
  max-width: calc(100% - 32px); padding: 6px 8px;
  background: var(--bg-2); border: 1px solid var(--border-hover);
  border-radius: var(--radius-sm); box-shadow: var(--shadow);
  font-family: 'Manrope', -apple-system, system-ui, sans-serif;
}
.findbar[hidden] { display: none; }
.findbar tf-input { width: 168px; }
.findbar tf-input input { padding: 4px 8px; font-size: 12px; }
.fb-count { font-size: 11px; color: var(--text-3); min-width: 48px; text-align: center; font-variant-numeric: tabular-nums; }
.fb-count.err { color: var(--danger); }
`;

export class TfCodeEditor extends HTMLElement {
  static get observedAttributes() {
    return ['language', 'readonly', 'wrap', 'tab-size', 'aria-label'];
  }

  constructor() {
    super();
    this._shadow = this.attachShadow({ mode: 'open' });

    // --- document model ---
    this._lines = [''];
    this._vers = [0];          // per-line version, bumped on change
    this._states = [''];       // tokenizer state BEFORE each line
    this._statesValidTo = 0;   // states[0.._statesValidTo] are valid
    this._tokCache = new Map();
    this._docGen = 0;
    this._cleanGen = 0;

    // --- selection ---
    this._anchor = { line: 0, ch: 0 };
    this._head = { line: 0, ch: 0 };
    this._goalX = null;        // preserved x for vertical caret movement
    this._bracket = null;      // [{line,ch},{line,ch}] matched bracket pair

    // --- folding ---
    this._folds = [];          // active folds {start, end} (hidden: start+1..end)
    this._foldable = new Map();
    this._foldableDirty = true;

    // --- layout / virtualization ---
    this._rowStart = new Int32Array(2);
    this._totalRows = 1;
    this._layoutDirty = true;
    this._expLen = [0];        // cached tab-expanded length per line
    this._contentW = 0;
    this._cwDirty = true;
    this._charW = 7.2;
    this._gutterW = 48;
    this._cols = 80;           // chars per visual row in wrap mode
    this._pool = [];           // content row divs
    this._gpool = [];          // gutter row divs
    this._renderScheduled = false;

    // --- undo/redo ---
    this._undo = [];
    this._redo = [];

    // --- search ---
    this._search = { open: false, query: '', regex: false, caseSense: false, replace: '' };
    this._matches = [];
    this._activeMatch = -1;
    this._searchError = false;
    this._searchTimer = null;

    // --- misc ---
    this._labels = { ...DEFAULT_LABELS };
    this._composing = false;
    this._changeTimer = null;
    this._selEventScheduled = false;
    this._tabEscapes = false;
    this._dragging = false;
    this._lastClick = { time: 0, count: 0, pos: null };

    this._onScroll = this._onScroll.bind(this);
    this._onWinMouseMove = this._onWinMouseMove.bind(this);
    this._onWinMouseUp = this._onWinMouseUp.bind(this);
  }

  // ------------------------------------------------------------------ setup

  connectedCallback() {
    if (!this._built) {
      this._built = true;
      this._build();
      adoptControlsInto(this._shadow);
    }
    this._measure();
    if (document.fonts?.ready) {
      document.fonts.ready.then(() => { this._measure(); this._scheduleRender(); });
    }
    this._ro = new ResizeObserver(() => { this._onResize(); });
    this._ro.observe(this._scroller);
    this._scheduleRender();
  }

  disconnectedCallback() {
    this._ro?.disconnect();
    this._ro = null;
    clearTimeout(this._changeTimer);
    clearTimeout(this._searchTimer);
    window.removeEventListener('mousemove', this._onWinMouseMove);
    window.removeEventListener('mouseup', this._onWinMouseUp);
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._built) return;
    if (name === 'language') {
      this._resetTokens();
      this._foldableDirty = true;
      // the indent unit (and thus tab expansion) may differ per language
      this._expLen = this._lines.map(() => -1);
      this._layoutDirty = true;
      this._cwDirty = true;
      this._scheduleRender();
    } else if (name === 'wrap' || name === 'tab-size') {
      this._expLen = this._lines.map(() => -1);
      this._layoutDirty = true;
      this._cwDirty = true;
      this._scheduleRender();
    } else if (name === 'readonly') {
      this._syncReadonlyUi();
    } else if (name === 'aria-label') {
      this._input.setAttribute('aria-label', newVal || this._labels.editor);
    }
  }

  _build() {
    const style = document.createElement('style');
    style.textContent = STYLE;
    this._shadow.appendChild(style);

    const editor = document.createElement('div');
    editor.className = 'editor';
    editor.setAttribute('part', 'editor');
    this._editorEl = editor;

    const scroller = document.createElement('div');
    scroller.className = 'scroller';
    this._scroller = scroller;

    const canvas = document.createElement('div');
    canvas.className = 'canvas';
    this._canvas = canvas;

    const gutter = document.createElement('div');
    gutter.className = 'gutter';
    gutter.setAttribute('aria-hidden', 'true');
    this._gutter = gutter;

    const content = document.createElement('div');
    content.className = 'content';
    this._content = content;

    // Presentation rows live in an aria-hidden wrapper; the hidden textarea
    // (the accessible surface) stays outside of it.
    const linesLayer = document.createElement('div');
    linesLayer.setAttribute('aria-hidden', 'true');
    this._linesLayer = linesLayer;
    content.appendChild(linesLayer);

    const caret = document.createElement('div');
    caret.className = 'caret';
    caret.setAttribute('aria-hidden', 'true');
    this._caret = caret;
    content.appendChild(caret);

    const input = document.createElement('textarea');
    input.className = 'hidden-input';
    input.setAttribute('aria-label', this.getAttribute('aria-label') || this._labels.editor);
    input.setAttribute('aria-multiline', 'true');
    input.setAttribute('autocapitalize', 'off');
    input.setAttribute('autocorrect', 'off');
    input.setAttribute('autocomplete', 'off');
    input.spellcheck = false;
    input.wrap = 'off';
    this._input = input;
    content.appendChild(input);

    canvas.appendChild(gutter);
    canvas.appendChild(content);
    scroller.appendChild(canvas);
    editor.appendChild(scroller);
    this._shadow.appendChild(editor);

    // measurement canvas
    this._mctx = document.createElement('canvas').getContext('2d');

    scroller.addEventListener('scroll', this._onScroll, { passive: true });
    content.addEventListener('mousedown', (e) => this._onMouseDown(e));
    gutter.addEventListener('mousedown', (e) => {
      // clicks on fold chevrons are handled per-row; plain gutter clicks select the line
      if (e.target.classList?.contains('g-fold')) return;
      e.preventDefault();
    });

    input.addEventListener('keydown', (e) => this._onKeyDown(e));
    input.addEventListener('input', () => this._onInput());
    input.addEventListener('compositionstart', () => { this._composing = true; });
    input.addEventListener('compositionend', () => {
      this._composing = false;
      this._onInput();
    });
    input.addEventListener('focus', () => { this._editorEl.classList.add('focused'); this._scheduleRender(); });
    input.addEventListener('blur', () => {
      this._editorEl.classList.remove('focused');
      this._flushChange();
      this._scheduleRender();
    });
  }

  // ------------------------------------------------------------- public API

  get value() { return this._lines.join('\n'); }

  set value(v) {
    const text = String(v ?? '');
    this._lines = text.split('\n');
    this._vers = this._lines.map(() => 0);
    this._states = [''];
    this._statesValidTo = 0;
    this._tokCache.clear();
    this._expLen = this._lines.map(() => -1);
    this._folds = [];
    this._foldableDirty = true;
    this._layoutDirty = true;
    this._cwDirty = true;
    this._undo = [];
    this._redo = [];
    this._docGen = 0;
    this._cleanGen = 0;
    this._matches = [];
    this._activeMatch = -1;
    this._lastEmittedGen = undefined;
    this._anchor = { line: 0, ch: 0 };
    this._head = { line: 0, ch: 0 };
    this._bracket = null;
    if (this._scroller) { this._scroller.scrollTop = 0; this._scroller.scrollLeft = 0; }
    if (this._search.open) this._scheduleSearch();
    this._scheduleRender();
  }

  get language() { return this.getAttribute('language') || 'plain'; }
  set language(v) { this.setAttribute('language', v || 'plain'); }

  get readOnly() { return this.hasAttribute('readonly'); }
  set readOnly(v) { v ? this.setAttribute('readonly', '') : this.removeAttribute('readonly'); }

  get wrap() { return this.hasAttribute('wrap'); }
  set wrap(v) { v ? this.setAttribute('wrap', '') : this.removeAttribute('wrap'); }

  get tabSize() {
    const attr = parseInt(this.getAttribute('tab-size') || '', 10);
    if (Number.isInteger(attr) && attr > 0) return attr;
    return INDENT_UNITS[this.language] ?? 4;
  }
  set tabSize(v) { this.setAttribute('tab-size', String(v)); }

  get dirty() { return this._docGen !== this._cleanGen; }

  get labels() { return this._labels; }
  set labels(dict) {
    this._labels = { ...DEFAULT_LABELS, ...(dict || {}) };
    this._input?.setAttribute('aria-label', this.getAttribute('aria-label') || this._labels.editor);
    if (this._findbar) this._applyFindbarLabels();
  }

  getSelection() {
    const [from, to] = this._ordered();
    return { from: copyPos(from), to: copyPos(to), text: this._getRange(from, to) };
  }

  setSelection(from, to) {
    const f = this._clampPos(from);
    const t = to ? this._clampPos(to) : copyPos(f);
    this._setSel(f, t);
    this._revealPos(this._head);
    this._scheduleRender();
  }

  replaceSelection(text) {
    if (this.readOnly) return;
    const [from, to] = this._ordered();
    this._edit(from, to, String(text ?? ''), 'api');
    this._revealPos(this._head);
  }

  gotoLine(line1) {
    const line = Math.max(0, Math.min(this._lines.length - 1, (line1 | 0) - 1));
    this._unfoldAt(line);
    this._setSel({ line, ch: 0 }, { line, ch: 0 });
    this._revealPos(this._head, true);
    this._scheduleRender();
  }

  markClean() { this._cleanGen = this._docGen; }

  focus() { this._input?.focus({ preventScroll: true }); }

  openSearch(withReplace = false) {
    this._ensureFindbar();
    this._search.open = true;
    this._findbar.hidden = false;
    const selText = this.getSelection().text;
    if (selText && !selText.includes('\n')) {
      this._fbFind.value = selText;
      this._search.query = selText;
    }
    this._syncReadonlyUi();
    this._scheduleSearch(0);
    (withReplace && !this.readOnly ? this._fbReplace : this._fbFind).focus();
  }

  foldAll() {
    this._ensureFoldable();
    this._folds = [];
    for (const [start, end] of this._foldable) this._folds.push({ start, end });
    this._folds.sort((a, b) => a.start - b.start);
    const [from] = this._ordered();
    this._unfoldAt(from.line);
    this._layoutDirty = true;
    this._scheduleRender();
  }

  unfoldAll() {
    if (!this._folds.length) return;
    this._folds = [];
    this._layoutDirty = true;
    this._scheduleRender();
  }

  // ------------------------------------------------------------ measurement

  _measure() {
    this._mctx.font = FONT;
    const w = this._mctx.measureText('XXXXXXXXXX').width / 10;
    if (w !== this._charW) {
      this._charW = w;
      this._cwDirty = true;
      if (this.wrap) this._layoutDirty = true;
    }
  }

  _textWidth(str) { return this._mctx.measureText(str).width; }

  _onResize() {
    if (this.wrap) {
      this._expLen = this._lines.map(() => -1);
      this._layoutDirty = true;
    }
    this._scheduleRender();
  }

  // Expand tabs to spaces at tab stops; used consistently for both rendering
  // and measurement, so x positions are exact by construction.
  _expand(text, startCol = 0) {
    if (!text.includes('\t')) return text;
    const ts = this.tabSize;
    let out = '', col = startCol;
    for (let i = 0; i < text.length; i++) {
      if (text[i] === '\t') {
        const n = ts - (col % ts);
        out += ' '.repeat(n);
        col += n;
      } else { out += text[i]; col++; }
    }
    return out;
  }

  _expandedLen(lineIdx) {
    let v = this._expLen[lineIdx];
    if (v === undefined || v < 0) {
      v = this._expand(this._lines[lineIdx]).length;
      this._expLen[lineIdx] = v;
    }
    return v;
  }

  // ------------------------------------------------------------- fold state

  _ensureFoldable() {
    if (!this._foldableDirty) return;
    const lang = this.language;
    this._foldable = (lang === 'javascript' || lang === 'typescript' || lang === 'json')
      ? computeBraceFolds(this._lines)
      : computeIndentFolds(this._lines, this.tabSize);
    // drop active folds that stopped being foldable (their region changed)
    const before = this._folds.length;
    this._folds = this._folds.filter((f) => {
      const end = this._foldable.get(f.start);
      if (end === undefined) return false;
      f.end = end;
      return true;
    });
    if (this._folds.length !== before) this._layoutDirty = true;
    this._foldableDirty = false;
  }

  _foldAt(line) {
    return this._folds.find((f) => f.start === line) || null;
  }

  _lineHidden(line) {
    return this._folds.some((f) => line > f.start && line <= f.end);
  }

  _unfoldAt(line) {
    const before = this._folds.length;
    this._folds = this._folds.filter((f) => !(line > f.start && line <= f.end));
    if (this._folds.length !== before) {
      this._layoutDirty = true;
      return true;
    }
    return false;
  }

  _toggleFold(line) {
    const active = this._foldAt(line);
    if (active) {
      this._folds = this._folds.filter((f) => f !== active);
    } else {
      this._ensureFoldable();
      const end = this._foldable.get(line);
      if (end === undefined) return;
      this._folds.push({ start: line, end });
      this._folds.sort((a, b) => a.start - b.start);
      const [from, to] = this._ordered();
      if (from.line > line && from.line <= end) this._setSel({ line, ch: this._lines[line].length });
      else if (to.line > line && to.line <= end) this._setSel(this._ordered()[0]);
    }
    this._layoutDirty = true;
    this._scheduleRender();
  }

  // ----------------------------------------------------------------- layout

  _rebuildLayout() {
    const n = this._lines.length;
    const hidden = new Uint8Array(n);
    for (const f of this._folds) {
      for (let i = f.start + 1; i <= f.end && i < n; i++) hidden[i] = 1;
    }
    const rowStart = new Int32Array(n + 1);
    const wrapOn = this.wrap;
    if (wrapOn) {
      const vw = this._scroller.clientWidth - this._gutterW - PAD_X * 2;
      this._cols = Math.max(16, Math.floor(vw / this._charW));
    }
    let acc = 0;
    for (let i = 0; i < n; i++) {
      rowStart[i] = acc;
      if (!hidden[i]) acc += wrapOn ? Math.max(1, Math.ceil(this._expandedLen(i) / this._cols)) : 1;
    }
    rowStart[n] = acc;
    this._rowStart = rowStart;
    this._totalRows = acc;
    this._hidden = hidden;
    this._layoutDirty = false;
  }

  _rowToLine(row) {
    const rs = this._rowStart;
    let lo = 0, hi = this._lines.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (rs[mid] <= row) lo = mid; else hi = mid - 1;
    }
    // skip hidden lines sharing the same rowStart (they contribute 0 rows)
    while (lo > 0 && this._hidden?.[lo]) lo--;
    return { line: lo, seg: row - rs[lo] };
  }

  _lineToRow(line) { return this._rowStart[Math.max(0, Math.min(line, this._lines.length))]; }

  // Segment boundaries (original char indices) for a wrapped line.
  _segBreaks(lineIdx) {
    const text = this._lines[lineIdx];
    if (!this.wrap) return [0];
    const ts = this.tabSize, cols = this._cols;
    const breaks = [0];
    let col = 0;
    for (let i = 0; i < text.length; i++) {
      const w = text[i] === '\t' ? ts - (col % ts) : 1;
      if (col + w > cols && col > 0) { breaks.push(i); col = 0; }
      col += w;
    }
    return breaks;
  }

  _posToRowX(pos) {
    const line = pos.line;
    const baseRow = this._lineToRow(line);
    const text = this._lines[line];
    const ch = Math.min(pos.ch, text.length);
    if (!this.wrap) {
      return { row: baseRow, x: PAD_X + this._textWidth(this._expand(text.slice(0, ch))) };
    }
    const breaks = this._segBreaks(line);
    let seg = 0;
    while (seg + 1 < breaks.length && breaks[seg + 1] <= ch) seg++;
    const segStart = breaks[seg];
    const prefixExpanded = this._expand(text.slice(0, segStart));
    const inSeg = this._expand(text.slice(segStart, ch), prefixExpanded.length);
    return { row: baseRow + seg, x: PAD_X + this._textWidth(inSeg) };
  }

  _posFromMouse(e) {
    const rect = this._content.getBoundingClientRect();
    const x = e.clientX - rect.left - PAD_X;
    const y = e.clientY - rect.top;
    let row = Math.floor(y / LINE_H);
    row = Math.max(0, Math.min(this._totalRows - 1, row));
    const { line, seg } = this._rowToLine(row);
    const text = this._lines[line];
    const breaks = this._segBreaks(line);
    const segStart = breaks[Math.min(seg, breaks.length - 1)];
    const segEnd = seg + 1 < breaks.length ? breaks[seg + 1] : text.length;
    const prefixCol = this._expand(text.slice(0, segStart)).length;
    // binary search for the closest column
    let lo = segStart, hi = segEnd;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      const w = this._textWidth(this._expand(text.slice(segStart, mid + 1), prefixCol));
      const wPrev = this._textWidth(this._expand(text.slice(segStart, mid), prefixCol));
      if ((w + wPrev) / 2 < x) lo = mid + 1; else hi = mid;
    }
    return { line, ch: lo };
  }

  // -------------------------------------------------------------- rendering

  _scheduleRender() {
    if (this._renderScheduled || !this._built) return;
    this._renderScheduled = true;
    requestAnimationFrame(() => {
      this._renderScheduled = false;
      this._render();
    });
  }

  _onScroll() { this._scheduleRender(); }

  _render() {
    if (!this.isConnected) return;
    this._ensureFoldable();
    if (this._layoutDirty) this._rebuildLayout();

    // gutter width follows the digit count
    const digits = String(this._lines.length).length;
    const gw = Math.ceil(8 + digits * this._charW + 16 + 4);
    if (gw !== this._gutterW) {
      this._gutterW = gw;
      this._gutter.style.width = `${gw}px`;
      if (this.wrap) { this._rebuildLayout(); }
    }

    // content width
    if (this._cwDirty && !this.wrap) {
      let maxLen = 0;
      for (let i = 0; i < this._lines.length; i++) {
        const l = this._expandedLen(i);
        if (l > maxLen) maxLen = l;
      }
      this._contentW = Math.ceil(maxLen * this._charW + PAD_X * 2 + 40);
      this._cwDirty = false;
    }
    const vpW = this._scroller.clientWidth;
    const contentW = this.wrap ? vpW - this._gutterW : Math.max(this._contentW, vpW - this._gutterW);
    const totalH = this._totalRows * LINE_H + LINE_H; // small bottom margin
    this._content.style.width = `${contentW}px`;
    this._content.style.height = `${totalH}px`;
    this._gutter.style.height = `${totalH}px`;

    const st = this._scroller.scrollTop;
    const vh = this._scroller.clientHeight;
    const first = Math.max(0, Math.floor(st / LINE_H) - OVERSCAN);
    const last = Math.min(this._totalRows - 1, Math.ceil((st + vh) / LINE_H) + OVERSCAN);
    const need = Math.max(0, last - first + 1);

    // ensure tokenizer states cover the visible range
    const lastLine = need > 0 ? this._rowToLine(last).line : 0;
    this._ensureStates(lastLine + 1);

    const cap = Math.max(need, Math.ceil(vh / LINE_H) + OVERSCAN * 2 + 2);
    this._ensurePools(cap);

    const [selFrom, selTo] = this._ordered();
    const selEmpty = posEq(selFrom, selTo);
    const caretRX = this._posToRowX(this._head);

    // hide pooled rows that fall outside the window
    for (let p = 0; p < this._pool.length; p++) {
      const div = this._pool[p];
      if (div._row === undefined || div._row < first || div._row > last) {
        if (div.style.display !== 'none') div.style.display = 'none';
        div._row = undefined;
        const g = this._gpool[p];
        if (g) { g.style.display = 'none'; g._row = undefined; }
      }
    }

    for (let row = first; row <= last; row++) {
      const idx = row % cap;
      const div = this._pool[idx];
      const gdiv = this._gpool[idx];
      const { line, seg } = this._rowToLine(row);
      this._renderContentRow(div, row, line, seg, contentW, selFrom, selTo, selEmpty, caretRX);
      this._renderGutterRow(gdiv, row, line, seg);
    }

    // caret + hidden input placement
    this._caret.style.transform = `translate(${caretRX.x}px, ${caretRX.row * LINE_H}px)`;
    this._input.style.transform = `translate(${Math.max(0, caretRX.x - 1)}px, ${caretRX.row * LINE_H}px)`;
  }

  _ensurePools(cap) {
    while (this._pool.length < cap) {
      const div = document.createElement('div');
      div.className = 'row';
      const bg = document.createElement('div');
      bg.className = 'row-bg';
      const txt = document.createElement('div');
      txt.className = 'row-text';
      div.appendChild(bg);
      div.appendChild(txt);
      div._bg = bg;
      div._txt = txt;
      this._linesLayer.appendChild(div);

      const g = document.createElement('div');
      g.className = 'gutter-row';
      const fold = document.createElement('span');
      fold.className = 'g-fold';
      const num = document.createElement('span');
      num.className = 'g-num';
      g.appendChild(num);
      g.appendChild(fold);
      g._num = num;
      g._fold = fold;
      fold.addEventListener('mousedown', (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (g._line !== undefined) this._toggleFold(g._line);
      });
      this._gutter.appendChild(g);
      this._gpool.push(g);
      this._pool.push(div);
    }
  }

  _tokensFor(line) {
    const key = `${line}:${this._vers[line]}:${this._states[line]}`;
    let entry = this._tokCache.get(key);
    if (!entry) {
      entry = tokenizeLine(this.language, this._lines[line], this._states[line]).toks;
      this._tokCache.set(key, entry);
      if (this._tokCache.size > 1200) {
        // drop the oldest half (Map preserves insertion order)
        let i = 0;
        const drop = this._tokCache.size - 600;
        for (const k of this._tokCache.keys()) {
          this._tokCache.delete(k);
          if (++i >= drop) break;
        }
      }
    }
    return entry;
  }

  _ensureStates(toLine) {
    const n = Math.min(toLine, this._lines.length - 1);
    while (this._statesValidTo < n) {
      const i = this._statesValidTo;
      const res = tokenizeLine(this.language, this._lines[i], this._states[i]);
      this._states[i + 1] = res.state;
      this._statesValidTo = i + 1;
    }
  }

  _resetTokens() {
    this._states = [''];
    this._statesValidTo = 0;
    this._tokCache.clear();
    for (const div of this._pool) div._tkey = null;
  }

  _renderContentRow(div, row, line, seg, contentW, selFrom, selTo, selEmpty, caretRX) {
    div.style.display = '';
    div._row = row;
    div.style.transform = `translateY(${row * LINE_H}px)`;
    div.style.width = `${contentW}px`;

    const text = this._lines[line];
    const breaks = this._segBreaks(line);
    const segStart = breaks[Math.min(seg, breaks.length - 1)];
    const segEnd = seg + 1 < breaks.length ? breaks[seg + 1] : text.length;
    const lastSeg = seg === breaks.length - 1;
    const fold = lastSeg ? this._foldAt(line) : null;

    // --- text layer (rebuilt only when the line/tokens/fold changed) ---
    const tkey = `${line}:${seg}:${this._vers[line]}:${this._states[line]}:${fold ? fold.end : ''}`;
    if (div._tkey !== tkey) {
      div._tkey = tkey;
      const txt = div._txt;
      txt.textContent = '';
      const toks = this._tokensFor(line);
      const prefixCol = segStart > 0 ? this._expand(text.slice(0, segStart)).length : 0;
      let pos = segStart, col = prefixCol;
      const emit = (from, to, cls) => {
        if (to <= from) return;
        const raw = text.slice(from, to);
        const expanded = this._expand(raw, col);
        col += expanded.length;
        if (cls) {
          const span = document.createElement('span');
          span.className = 'tk-' + cls;
          span.textContent = expanded;
          txt.appendChild(span);
        } else {
          txt.appendChild(document.createTextNode(expanded));
        }
      };
      for (let t = 0; t < toks.length; t += 3) {
        const ts = Math.max(toks[t], segStart);
        const te = Math.min(toks[t + 1], segEnd);
        if (te <= pos) continue;
        if (ts >= segEnd) break;
        emit(pos, Math.max(pos, ts), null);
        emit(Math.max(pos, ts), te, toks[t + 2]);
        pos = te;
      }
      emit(pos, segEnd, null);
      if (fold) {
        const pill = document.createElement('span');
        pill.className = 'fold-pill';
        pill.textContent = `⋯ ${fmt(this._labels.folded_lines, { n: fold.end - fold.start })}`;
        pill.addEventListener('mousedown', (e) => {
          e.preventDefault();
          e.stopPropagation();
          this._toggleFold(line);
        });
        txt.appendChild(pill);
      }
    }

    // --- background layer: current line, selection, search, bracket ---
    const rects = [];
    if (selEmpty && this._head.line === line && caretRX.row === row) {
      rects.push(['cur-line', 0, 0]);
    }
    if (!selEmpty && !(selTo.line < line || selFrom.line > line)) {
      const a = selFrom.line === line ? Math.max(selFrom.ch, segStart) : segStart;
      const b = selTo.line === line ? Math.min(selTo.ch, segEnd) : segEnd;
      if (b >= a) {
        const prefixCol = this._expand(text.slice(0, segStart)).length;
        const x1 = PAD_X + this._textWidth(this._expand(text.slice(segStart, a), prefixCol));
        let x2 = PAD_X + this._textWidth(this._expand(text.slice(segStart, b), prefixCol));
        // selection spilling past the line end marks the newline
        if (selTo.line > line && lastSeg) x2 += this._charW * 0.6;
        if (x2 - x1 < 1 && selTo.line > line) x2 = x1 + this._charW * 0.6;
        if (x2 > x1) rects.push(['sel', x1, x2 - x1]);
      }
    }
    if (this._search.open && this._matches.length) {
      const lo = this._lowerBoundMatch(line);
      for (let m = lo; m < this._matches.length && this._matches[m].line === line; m++) {
        const mt = this._matches[m];
        const a = Math.max(mt.start, segStart), b = Math.min(mt.end, segEnd);
        if (b <= a) continue;
        const prefixCol = this._expand(text.slice(0, segStart)).length;
        const x1 = PAD_X + this._textWidth(this._expand(text.slice(segStart, a), prefixCol));
        const x2 = PAD_X + this._textWidth(this._expand(text.slice(segStart, b), prefixCol));
        rects.push([m === this._activeMatch ? 'match-active' : 'match', x1, x2 - x1]);
      }
    }
    if (this._bracket) {
      for (const bp of this._bracket) {
        if (bp.line !== line || bp.ch < segStart || bp.ch >= segEnd) continue;
        const prefixCol = this._expand(text.slice(0, segStart)).length;
        const x1 = PAD_X + this._textWidth(this._expand(text.slice(segStart, bp.ch), prefixCol));
        const x2 = PAD_X + this._textWidth(this._expand(text.slice(segStart, bp.ch + 1), prefixCol));
        rects.push(['bracket', x1, x2 - x1]);
      }
    }
    const bkey = rects.map((r) => `${r[0]},${r[1].toFixed(1)},${r[2].toFixed(1)}`).join('|');
    if (div._bkey !== bkey) {
      div._bkey = bkey;
      const bg = div._bg;
      bg.textContent = '';
      for (const [cls, x, w] of rects) {
        const r = document.createElement('div');
        r.className = 'rect ' + cls;
        if (cls !== 'cur-line') {
          r.style.left = `${x}px`;
          r.style.width = `${Math.max(1, w)}px`;
        }
        bg.appendChild(r);
      }
    }
  }

  _renderGutterRow(g, row, line, seg) {
    g.style.display = '';
    g._row = row;
    g._line = line;
    g.style.transform = `translateY(${row * LINE_H}px)`;
    const isFirst = seg === 0;
    const numText = isFirst ? String(line + 1) : '';
    if (g._num.textContent !== numText) g._num.textContent = numText;
    const active = this._head.line === line;
    g._num.classList.toggle('active', active);
    const folded = !!this._foldAt(line);
    const can = isFirst && (folded || this._foldable.has(line));
    g._fold.classList.toggle('can', can);
    g._fold.classList.toggle('folded', folded);
    const glyph = can ? (folded ? '▸' : '▾') : '';
    if (g._fold.textContent !== glyph) g._fold.textContent = glyph;
  }

  _lowerBoundMatch(line) {
    let lo = 0, hi = this._matches.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (this._matches[mid].line < line) lo = mid + 1; else hi = mid;
    }
    return lo;
  }

  // -------------------------------------------------------------- selection

  _ordered() {
    return cmpPos(this._anchor, this._head) <= 0
      ? [this._anchor, this._head]
      : [this._head, this._anchor];
  }

  _clampPos(pos) {
    const line = Math.max(0, Math.min(this._lines.length - 1, pos?.line | 0));
    const ch = Math.max(0, Math.min(this._lines[line].length, pos?.ch | 0));
    return { line, ch };
  }

  _setSel(head, anchor) {
    this._head = this._clampPos(head);
    this._anchor = anchor ? this._clampPos(anchor) : copyPos(this._head);
    this._afterSelChange();
  }

  _moveHead(pos, extend) {
    this._head = this._clampPos(pos);
    if (!extend) this._anchor = copyPos(this._head);
    this._afterSelChange();
  }

  _afterSelChange() {
    this._goalX = null;
    this._updateBracket();
    this._syncHiddenInput();
    this._scheduleRender();
    if (!this._selEventScheduled) {
      this._selEventScheduled = true;
      requestAnimationFrame(() => {
        this._selEventScheduled = false;
        const [from, to] = this._ordered();
        this.dispatchEvent(new CustomEvent('selection-change', {
          bubbles: true, composed: true,
          detail: { from: copyPos(from), to: copyPos(to), text: this._getRange(from, to) },
        }));
      });
    }
  }

  _syncHiddenInput() {
    if (this._composing || this._dragging) return;
    const [from, to] = this._ordered();
    const text = posEq(from, to) ? '' : this._getRange(from, to);
    if (this._input.value !== text) this._input.value = text;
    this._input.setSelectionRange(0, text.length);
  }

  _getRange(from, to) {
    if (from.line === to.line) return this._lines[from.line].slice(from.ch, to.ch);
    const parts = [this._lines[from.line].slice(from.ch)];
    for (let i = from.line + 1; i < to.line; i++) parts.push(this._lines[i]);
    parts.push(this._lines[to.line].slice(0, to.ch));
    return parts.join('\n');
  }

  _revealPos(pos, center = false) {
    if (this._layoutDirty) { this._ensureFoldable(); this._rebuildLayout(); }
    const { row, x } = this._posToRowX(pos);
    const sc = this._scroller;
    const top = row * LINE_H;
    if (center) {
      sc.scrollTop = Math.max(0, top - sc.clientHeight / 2);
    } else if (top < sc.scrollTop) {
      sc.scrollTop = top;
    } else if (top + LINE_H > sc.scrollTop + sc.clientHeight) {
      sc.scrollTop = top + LINE_H - sc.clientHeight;
    }
    const viewX = this._gutterW + x;
    if (viewX < sc.scrollLeft + this._gutterW + 8) {
      sc.scrollLeft = Math.max(0, x - 24);
    } else if (viewX > sc.scrollLeft + sc.clientWidth - 12) {
      sc.scrollLeft = viewX - sc.clientWidth + 48;
    }
  }

  _updateBracket() {
    this._bracket = null;
    if (!posEq(this._anchor, this._head)) return;
    const { line, ch } = this._head;
    const text = this._lines[line];
    for (const at of [ch - 1, ch]) {
      const c = text[at];
      if (!c) continue;
      if (PAIRS[c]) {
        const match = this._scanBracket(line, at, c, PAIRS[c], 1);
        if (match) this._bracket = [{ line, ch: at }, match];
        return;
      }
      if (CLOSERS[c]) {
        const match = this._scanBracket(line, at, c, CLOSERS[c], -1);
        if (match) this._bracket = [{ line, ch: at }, match];
        return;
      }
    }
  }

  _scanBracket(line, ch, open, close, dir) {
    let depth = 0, scanned = 0;
    let l = line, i = ch;
    while (l >= 0 && l < this._lines.length && scanned < BRACKET_SCAN_LIMIT) {
      const text = this._lines[l];
      while (i >= 0 && i < text.length) {
        const c = text[i];
        if (c === open) depth++;
        else if (c === close) {
          depth--;
          if (depth === 0) return { line: l, ch: i };
        }
        i += dir;
        scanned++;
      }
      l += dir;
      if (l >= 0 && l < this._lines.length) i = dir > 0 ? 0 : this._lines[l].length - 1;
    }
    return null;
  }

  // ------------------------------------------------------------------ edits

  // Low-level buffer splice. Returns { removed, end }.
  _replaceRange(from, to, text) {
    const removed = this._getRange(from, to);
    const before = this._lines[from.line].slice(0, from.ch);
    const after = this._lines[to.line].slice(to.ch);
    const parts = text.split('\n');
    const newLines = parts.length === 1
      ? [before + parts[0] + after]
      : [before + parts[0], ...parts.slice(1, -1), parts[parts.length - 1] + after];
    const oldCount = to.line - from.line + 1;
    this._lines.splice(from.line, oldCount, ...newLines);
    const newVers = newLines.map((_, k) => ((this._vers[from.line + k] || 0) + 1));
    this._vers.splice(from.line, oldCount, ...newVers);
    this._expLen.splice(from.line, oldCount, ...newLines.map(() => -1));

    this._statesValidTo = Math.min(this._statesValidTo, from.line);
    this._states.length = Math.min(this._states.length, from.line + 1);

    const delta = newLines.length - oldCount;
    // folds: drop any whose hidden region the edit touches, shift the rest
    this._folds = this._folds.filter((f) => !(from.line <= f.end && to.line >= f.start + 1));
    if (delta !== 0) {
      for (const f of this._folds) {
        if (f.start > to.line) { f.start += delta; f.end += delta; }
        else if (f.end > to.line) { f.end += delta; }
      }
    }

    if (delta !== 0 || this.wrap) this._layoutDirty = true;
    this._cwDirty = true;
    this._foldableDirty = true;
    this._docGen++;
    const end = advancePos(from, text);
    return { removed, end };
  }

  // Applies one edit as an undo entry (with typing/backspace coalescing).
  _edit(from, to, text, kind) {
    if (this.readOnly) return;
    const selBefore = { anchor: copyPos(this._anchor), head: copyPos(this._head) };
    const { removed, end } = this._replaceRange(from, to, text);
    this._setSel(end);
    const selAfter = { anchor: copyPos(this._anchor), head: copyPos(this._head) };
    const step = { from: copyPos(from), to: copyPos(to), inserted: text, removed };
    const now = performance.now();
    const last = this._undo[this._undo.length - 1];
    let coalesced = false;
    if (last && last.kind === kind && now - last.time < 700 && last.steps.length === 1) {
      const ls = last.steps[0];
      if (kind === 'type' && !text.includes('\n') && posEq(from, to)
          && posEq(from, advancePos(ls.from, ls.inserted)) && ls.removed === '') {
        ls.inserted += text;
        last.after = selAfter;
        last.time = now;
        coalesced = true;
      } else if (kind === 'del-back' && text === '' && posEq(to, ls.from) && !removed.includes('\n')) {
        ls.from = copyPos(from);
        ls.removed = removed + ls.removed;
        last.after = selAfter;
        last.time = now;
        coalesced = true;
      }
    }
    if (!coalesced) {
      this._undo.push({ kind, time: now, steps: [step], before: selBefore, after: selAfter });
      if (this._undo.length > 500) this._undo.shift();
    }
    this._redo = [];
    this._emitChangeSoon();
    this._scheduleRender();
  }

  // Multi-step edit as a single undo entry. steps: [{from, to, text}] applied
  // in order (positions must be valid at application time).
  _editGroup(steps, kind, selAfterFn) {
    if (this.readOnly || !steps.length) return;
    const selBefore = { anchor: copyPos(this._anchor), head: copyPos(this._head) };
    const applied = [];
    for (const s of steps) {
      const { removed } = this._replaceRange(s.from, s.to, s.text);
      applied.push({ from: copyPos(s.from), to: copyPos(s.to), inserted: s.text, removed });
    }
    if (selAfterFn) selAfterFn();
    const selAfter = { anchor: copyPos(this._anchor), head: copyPos(this._head) };
    this._undo.push({ kind, time: performance.now(), steps: applied, before: selBefore, after: selAfter });
    if (this._undo.length > 500) this._undo.shift();
    this._redo = [];
    this._emitChangeSoon();
    this._scheduleRender();
  }

  _undoOnce() {
    const entry = this._undo.pop();
    if (!entry) return;
    for (let i = entry.steps.length - 1; i >= 0; i--) {
      const s = entry.steps[i];
      this._replaceRange(s.from, advancePos(s.from, s.inserted), s.removed);
    }
    this._redo.push(entry);
    this._anchor = copyPos(entry.before.anchor);
    this._head = copyPos(entry.before.head);
    this._afterSelChange();
    this._revealPos(this._head);
    this._emitChangeSoon();
    this._scheduleRender();
  }

  _redoOnce() {
    const entry = this._redo.pop();
    if (!entry) return;
    for (const s of entry.steps) {
      this._replaceRange(s.from, advancePos(s.from, s.removed), s.inserted);
    }
    this._undo.push(entry);
    this._anchor = copyPos(entry.after.anchor);
    this._head = copyPos(entry.after.head);
    this._afterSelChange();
    this._revealPos(this._head);
    this._emitChangeSoon();
    this._scheduleRender();
  }

  _emitChangeSoon() {
    clearTimeout(this._changeTimer);
    this._changeTimer = setTimeout(() => this._flushChange(), 250);
    if (this._search.open) this._scheduleSearch();
  }

  _flushChange() {
    if (this._changeTimer === null) return;
    clearTimeout(this._changeTimer);
    this._changeTimer = null;
    if (this._lastEmittedGen === this._docGen) return;
    this._lastEmittedGen = this._docGen;
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true, composed: true,
      detail: { value: this.value, dirty: this.dirty },
    }));
  }

  // ------------------------------------------------------------------ mouse

  _onMouseDown(e) {
    if (e.button !== 0) return;
    if (e.target.classList?.contains('fold-pill')) return;
    e.preventDefault();
    this.focus();
    const pos = this._posFromMouse(e);
    const now = performance.now();
    const lc = this._lastClick;
    if (now - lc.time < 400 && lc.pos && posEq(lc.pos, pos)) lc.count++;
    else lc.count = 1;
    lc.time = now;
    lc.pos = copyPos(pos);

    if (lc.count >= 3) {
      const line = pos.line;
      const endPos = line + 1 < this._lines.length
        ? { line: line + 1, ch: 0 }
        : { line, ch: this._lines[line].length };
      this._setSel(endPos, { line, ch: 0 });
      return;
    }
    if (lc.count === 2) {
      const [a, b] = this._wordAt(pos);
      this._setSel(b, a);
      return;
    }
    this._moveHead(pos, e.shiftKey);
    this._dragging = true;
    window.addEventListener('mousemove', this._onWinMouseMove);
    window.addEventListener('mouseup', this._onWinMouseUp);
  }

  _onWinMouseMove(e) {
    if (!this._dragging) return;
    const rect = this._scroller.getBoundingClientRect();
    // auto-scroll when dragging beyond the viewport edges
    if (e.clientY < rect.top) this._scroller.scrollTop -= Math.min(40, rect.top - e.clientY);
    else if (e.clientY > rect.bottom) this._scroller.scrollTop += Math.min(40, e.clientY - rect.bottom);
    if (e.clientX < rect.left + this._gutterW) this._scroller.scrollLeft -= 20;
    else if (e.clientX > rect.right) this._scroller.scrollLeft += 20;
    this._moveHead(this._posFromMouse(e), true);
  }

  _onWinMouseUp() {
    this._dragging = false;
    window.removeEventListener('mousemove', this._onWinMouseMove);
    window.removeEventListener('mouseup', this._onWinMouseUp);
    this._syncHiddenInput();
  }

  _wordAt(pos) {
    const text = this._lines[pos.line];
    let a = pos.ch, b = pos.ch;
    if (a > 0 && !WORD_RE.test(text[a] || ' ') && WORD_RE.test(text[a - 1])) { a--; b--; }
    while (a > 0 && WORD_RE.test(text[a - 1])) a--;
    while (b < text.length && WORD_RE.test(text[b])) b++;
    if (a === b && b < text.length) b++;
    return [{ line: pos.line, ch: a }, { line: pos.line, ch: b }];
  }

  // --------------------------------------------------------------- keyboard

  _onKeyDown(e) {
    if (this._composing) return;
    const mac = /Mac|iPhone|iPad/.test(navigator.platform);
    const mod = mac ? e.metaKey : e.ctrlKey;
    const key = e.key;

    if (this._tabEscapes && key === 'Tab') { this._tabEscapes = false; return; }
    if (key !== 'Escape') this._tabEscapes = false;

    // shortcuts
    if (mod && !e.altKey) {
      switch (key.toLowerCase()) {
        case 'z':
          e.preventDefault();
          if (e.shiftKey) this._redoOnce(); else this._undoOnce();
          return;
        case 'y':
          if (!mac) { e.preventDefault(); this._redoOnce(); return; }
          break;
        case 'a':
          e.preventDefault();
          this._setSel(
            { line: this._lines.length - 1, ch: this._lines[this._lines.length - 1].length },
            { line: 0, ch: 0 },
          );
          return;
        case 'f':
          e.preventDefault();
          this.openSearch(false);
          return;
        case 'h':
          e.preventDefault();
          this.openSearch(true);
          return;
        case 's':
          e.preventDefault();
          this._flushChange();
          this.dispatchEvent(new CustomEvent('save', {
            bubbles: true, composed: true, detail: { value: this.value },
          }));
          return;
        default: break;
      }
    }

    switch (key) {
      case 'ArrowLeft':
      case 'ArrowRight': {
        e.preventDefault();
        const dir = key === 'ArrowLeft' ? -1 : 1;
        this._goalX = null;
        if (mac && e.metaKey) { this._moveLineEdge(dir, e.shiftKey); return; }
        if ((mac && e.altKey) || (!mac && e.ctrlKey)) { this._moveWord(dir, e.shiftKey); return; }
        this._moveChar(dir, e.shiftKey);
        return;
      }
      case 'ArrowUp':
      case 'ArrowDown': {
        e.preventDefault();
        this._moveVert(key === 'ArrowUp' ? -1 : 1, e.shiftKey);
        return;
      }
      case 'Home':
        e.preventDefault();
        if (e.ctrlKey && !mac) this._moveHead({ line: 0, ch: 0 }, e.shiftKey);
        else this._moveLineEdge(-1, e.shiftKey);
        this._revealPos(this._head);
        return;
      case 'End': {
        e.preventDefault();
        if (e.ctrlKey && !mac) {
          const ll = this._lines.length - 1;
          this._moveHead({ line: ll, ch: this._lines[ll].length }, e.shiftKey);
        } else this._moveLineEdge(1, e.shiftKey);
        this._revealPos(this._head);
        return;
      }
      case 'PageUp':
      case 'PageDown': {
        e.preventDefault();
        const page = Math.max(1, Math.floor(this._scroller.clientHeight / LINE_H) - 2);
        this._moveVert(key === 'PageUp' ? -page : page, e.shiftKey);
        return;
      }
      case 'Backspace':
        e.preventDefault();
        this._deleteDir(-1, (mac && e.altKey) || (!mac && e.ctrlKey));
        return;
      case 'Delete':
        e.preventDefault();
        this._deleteDir(1, (mac && e.altKey) || (!mac && e.ctrlKey));
        return;
      case 'Tab':
        e.preventDefault();
        this._handleTab(e.shiftKey);
        return;
      case 'Enter':
        e.preventDefault();
        this._handleEnter();
        return;
      case 'Escape':
        if (this._search.open) {
          e.preventDefault();
          this._closeSearch();
          return;
        }
        // accessibility escape hatch: Esc then Tab moves focus out
        this._tabEscapes = true;
        return;
      default:
        break;
    }
  }

  _moveChar(dir, extend) {
    const [from, to] = this._ordered();
    if (!extend && !posEq(from, to)) {
      this._moveHead(dir < 0 ? from : to, false);
      this._revealPos(this._head);
      return;
    }
    let { line, ch } = this._head;
    if (dir < 0) {
      if (ch > 0) ch--;
      else if (line > 0) { line = this._prevVisibleLine(line); ch = this._lines[line].length; }
    } else {
      if (ch < this._lines[line].length) ch++;
      else if (line < this._lines.length - 1) { line = this._nextVisibleLine(line); ch = 0; }
    }
    this._moveHead({ line, ch }, extend);
    this._revealPos(this._head);
  }

  _prevVisibleLine(line) {
    let l = line - 1;
    while (l > 0 && this._lineHidden(l)) l--;
    return Math.max(0, l);
  }

  _nextVisibleLine(line) {
    const fold = this._foldAt(line);
    let l = (fold ? fold.end : line) + 1;
    while (l < this._lines.length - 1 && this._lineHidden(l)) l++;
    return Math.min(this._lines.length - 1, l);
  }

  _moveWord(dir, extend) {
    let { line, ch } = this._head;
    const text = this._lines[line];
    if (dir < 0) {
      if (ch === 0) { this._moveChar(-1, extend); return; }
      let i = ch;
      while (i > 0 && !WORD_RE.test(text[i - 1])) i--;
      while (i > 0 && WORD_RE.test(text[i - 1])) i--;
      this._moveHead({ line, ch: i }, extend);
    } else {
      if (ch >= text.length) { this._moveChar(1, extend); return; }
      let i = ch;
      while (i < text.length && !WORD_RE.test(text[i])) i++;
      while (i < text.length && WORD_RE.test(text[i])) i++;
      this._moveHead({ line, ch: i }, extend);
    }
    this._revealPos(this._head);
  }

  _moveLineEdge(dir, extend) {
    const { line, ch } = this._head;
    if (dir < 0) {
      const text = this._lines[line];
      const first = text.length - text.trimStart().length;
      this._moveHead({ line, ch: ch === first ? 0 : first }, extend);
    } else {
      this._moveHead({ line, ch: this._lines[line].length }, extend);
    }
  }

  _moveVert(delta, extend) {
    if (this._layoutDirty) { this._ensureFoldable(); this._rebuildLayout(); }
    const cur = this._posToRowX(this._head);
    if (this._goalX === null) this._goalX = cur.x;
    const targetRow = Math.max(0, Math.min(this._totalRows - 1, cur.row + delta));
    if (targetRow === cur.row) {
      // hit the document edge: snap to start/end
      const pos = delta < 0
        ? { line: 0, ch: 0 }
        : { line: this._lines.length - 1, ch: this._lines[this._lines.length - 1].length };
      this._moveHead(pos, extend);
      this._revealPos(this._head);
      return;
    }
    const { line, seg } = this._rowToLine(targetRow);
    const pos = this._posFromRowX(line, seg, this._goalX);
    const gx = this._goalX;
    this._moveHead(pos, extend);
    this._goalX = gx; // _moveHead clears goal via _afterSelChange? no — keep explicit
    this._revealPos(this._head);
  }

  _posFromRowX(line, seg, x) {
    const text = this._lines[line];
    const breaks = this._segBreaks(line);
    const segStart = breaks[Math.min(seg, breaks.length - 1)];
    const segEnd = seg + 1 < breaks.length ? breaks[seg + 1] : text.length;
    const prefixCol = this._expand(text.slice(0, segStart)).length;
    const rel = x - PAD_X;
    let lo = segStart, hi = segEnd;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      const w = this._textWidth(this._expand(text.slice(segStart, mid + 1), prefixCol));
      const wPrev = this._textWidth(this._expand(text.slice(segStart, mid), prefixCol));
      if ((w + wPrev) / 2 < rel) lo = mid + 1; else hi = mid;
    }
    return { line, ch: lo };
  }

  _deleteDir(dir, byWord) {
    if (this.readOnly) return;
    const [from, to] = this._ordered();
    if (!posEq(from, to)) { this._edit(from, to, '', 'del'); this._revealPos(this._head); return; }
    let { line, ch } = this._head;
    if (dir < 0) {
      if (ch === 0) {
        if (line === 0) return;
        const prev = line - 1;
        this._edit({ line: prev, ch: this._lines[prev].length }, { line, ch: 0 }, '', 'del');
      } else if (byWord) {
        const save = copyPos(this._head);
        this._moveWord(-1, false);
        const target = copyPos(this._head);
        this._head = save; this._anchor = copyPos(save);
        this._edit(target, save, '', 'del');
      } else {
        const text = this._lines[line];
        // pair deletion: backspace between an auto-closable pair removes both
        const prevCh = text[ch - 1], nextCh = text[ch];
        if ((PAIRS[prevCh] === nextCh) || (QUOTES.has(prevCh) && prevCh === nextCh)) {
          this._edit({ line, ch: ch - 1 }, { line, ch: ch + 1 }, '', 'del');
        } else if (prevCh === ' ' && /^\s+$/.test(text.slice(0, ch))) {
          // soft-tab outdent: delete back to the previous tab stop
          const ts = this.tabSize;
          const target = Math.max(0, ch - (ch % ts === 0 ? ts : ch % ts));
          this._edit({ line, ch: target }, { line, ch }, '', 'del-back');
        } else {
          this._edit({ line, ch: ch - 1 }, { line, ch }, '', 'del-back');
        }
      }
    } else {
      const text = this._lines[line];
      if (ch >= text.length) {
        if (line >= this._lines.length - 1) return;
        this._edit({ line, ch }, { line: line + 1, ch: 0 }, '', 'del');
      } else if (byWord) {
        const save = copyPos(this._head);
        this._moveWord(1, false);
        const target = copyPos(this._head);
        this._head = save; this._anchor = copyPos(save);
        this._edit(save, target, '', 'del');
      } else {
        this._edit({ line, ch }, { line, ch: ch + 1 }, '', 'del');
      }
    }
    this._revealPos(this._head);
  }

  _handleTab(shift) {
    if (this.readOnly) return;
    const [from, to] = this._ordered();
    const unit = this.tabSize;
    const multiline = from.line !== to.line;
    if (multiline || shift) {
      // indent/outdent whole lines
      const lastLine = to.ch === 0 && to.line > from.line ? to.line - 1 : to.line;
      const steps = [];
      for (let l = from.line; l <= lastLine; l++) {
        const text = this._lines[l];
        if (shift) {
          const ws = /^[ \t]*/.exec(text)[0];
          let remove = 0, col = 0;
          for (let i = 0; i < ws.length && col < unit; i++) {
            col += ws[i] === '\t' ? unit : 1;
            remove++;
          }
          if (remove > 0) steps.push({ from: { line: l, ch: 0 }, to: { line: l, ch: remove }, text: '' });
        } else if (text.length > 0) {
          steps.push({ from: { line: l, ch: 0 }, to: { line: l, ch: 0 }, text: ' '.repeat(unit) });
        }
      }
      if (!steps.length) return;
      const anchorLine = this._anchor.line, headLine = this._head.line;
      this._editGroup(steps, 'indent', () => {
        const adj = (pos, lineNo) => {
          const text = this._lines[lineNo];
          return { line: lineNo, ch: Math.max(0, Math.min(text.length, pos.ch + (shift ? -unit : unit))) };
        };
        this._anchor = this._clampPos(adj(this._anchor, anchorLine));
        this._head = this._clampPos(adj(this._head, headLine));
        this._afterSelChange();
      });
      return;
    }
    // plain Tab: spaces to the next tab stop
    const col = this._expand(this._lines[from.line].slice(0, from.ch)).length;
    const n = unit - (col % unit);
    this._edit(from, to, ' '.repeat(n), 'type');
    this._revealPos(this._head);
  }

  _handleEnter() {
    if (this.readOnly) return;
    const [from, to] = this._ordered();
    const text = this._lines[from.line];
    const baseIndent = /^[ \t]*/.exec(text)[0];
    const beforeCaret = text.slice(0, from.ch).trimEnd();
    const lastCh = beforeCaret[beforeCaret.length - 1] || '';
    const nextCh = this._lines[to.line][to.ch] || '';
    const unit = ' '.repeat(this.tabSize);
    const lang = this.language;

    let extra = '';
    if ((lang === 'python' || lang === 'yaml') && lastCh === ':') extra = unit;
    else if (PAIRS[lastCh]) extra = unit;

    if (PAIRS[lastCh] && nextCh === PAIRS[lastCh]) {
      // Enter between brackets: closer moves to its own line, caret in between
      this._edit(from, to, '\n' + baseIndent + unit + '\n' + baseIndent, 'newline');
      const line = from.line + 1;
      this._setSel({ line, ch: this._lines[line].length });
    } else {
      this._edit(from, to, '\n' + baseIndent + extra, 'newline');
    }
    this._revealPos(this._head);
  }

  // ---------------------------------------------------------------- textarea

  _onInput() {
    if (this._composing) return;
    const inserted = this._input.value;
    const [from, to] = this._ordered();
    if (this.readOnly) { this._syncHiddenInput(); return; }
    if (inserted === '' && posEq(from, to)) return;
    if (inserted === this._getRange(from, to) && !posEq(from, to)) return; // no-op (e.g. plain copy)

    if (inserted.length === 1) {
      const ch = inserted;
      const nextCh = this._lines[to.line][to.ch] || '';
      // type-over a closing char that already sits at the caret
      if (posEq(from, to) && (CLOSERS[ch] || QUOTES.has(ch)) && nextCh === ch) {
        this._moveHead({ line: to.line, ch: to.ch + 1 }, false);
        this._revealPos(this._head);
        this._syncHiddenInput();
        return;
      }
      // backtick auto-closes only where template literals exist
      const lang = this.language;
      const quoteOk = ch === '`' ? (lang === 'javascript' || lang === 'typescript' || lang === 'markdown') : QUOTES.has(ch);
      const closer = PAIRS[ch] || (quoteOk ? ch : null);
      if (closer) {
        if (!posEq(from, to)) {
          // wrap the selection in the pair and keep it selected
          const selText = this._getRange(from, to);
          this._edit(from, to, ch + selText + closer, 'wrap-pair');
          this._setSel({ line: this._head.line, ch: this._head.ch - 1 }, advancePos(from, ch));
          this._revealPos(this._head);
          this._syncHiddenInput();
          return;
        }
        const prevCh = this._lines[from.line][from.ch - 1] || '';
        // never auto-close right before a word character; quotes additionally
        // stay single after a word character (apostrophes inside words)
        const blocked = WORD_RE.test(nextCh)
          || (!PAIRS[ch] && (WORD_RE.test(prevCh) || prevCh === ch));
        if (!blocked) {
          this._edit(from, to, ch + closer, 'type');
          this._setSel({ line: this._head.line, ch: this._head.ch - 1 });
          this._revealPos(this._head);
          this._syncHiddenInput();
          return;
        }
      }
    }
    this._edit(from, to, inserted, inserted.length === 1 ? 'type' : 'paste');
    this._revealPos(this._head);
    this._syncHiddenInput();
  }

  // ------------------------------------------------------------------ search

  _ensureFindbar() {
    if (this._findbar) return;
    const bar = document.createElement('div');
    bar.className = 'findbar';
    bar.hidden = true;

    const find = document.createElement('tf-input');
    find.className = 'fb-find';
    const caseBtn = document.createElement('tf-button');
    caseBtn.setAttribute('size', 'sm');
    caseBtn.textContent = 'Aa';
    const regexBtn = document.createElement('tf-button');
    regexBtn.setAttribute('size', 'sm');
    regexBtn.textContent = '.*';
    const count = document.createElement('span');
    count.className = 'fb-count';
    const prevBtn = document.createElement('tf-button');
    prevBtn.setAttribute('size', 'sm');
    prevBtn.setAttribute('variant', 'ghost');
    prevBtn.textContent = '↑';
    const nextBtn = document.createElement('tf-button');
    nextBtn.setAttribute('size', 'sm');
    nextBtn.setAttribute('variant', 'ghost');
    nextBtn.textContent = '↓';
    const replace = document.createElement('tf-input');
    replace.className = 'fb-replace';
    const replBtn = document.createElement('tf-button');
    replBtn.setAttribute('size', 'sm');
    const replAllBtn = document.createElement('tf-button');
    replAllBtn.setAttribute('size', 'sm');
    const closeBtn = document.createElement('tf-button');
    closeBtn.setAttribute('size', 'sm');
    closeBtn.setAttribute('variant', 'ghost');
    closeBtn.textContent = '✕';

    bar.append(find, caseBtn, regexBtn, count, prevBtn, nextBtn, replace, replBtn, replAllBtn, closeBtn);
    this._editorEl.appendChild(bar);
    this._findbar = bar;
    this._fbFind = find;
    this._fbReplace = replace;
    this._fbCase = caseBtn;
    this._fbRegex = regexBtn;
    this._fbCount = count;
    this._fbRepl = replBtn;
    this._fbReplAll = replAllBtn;
    this._applyFindbarLabels();

    find.addEventListener('input', () => {
      this._search.query = find.value;
      this._scheduleSearch();
    });
    replace.addEventListener('input', () => { this._search.replace = replace.value; });
    caseBtn.addEventListener('click', () => {
      this._search.caseSense = !this._search.caseSense;
      this._syncToggleButtons();
      this._scheduleSearch(0);
    });
    regexBtn.addEventListener('click', () => {
      this._search.regex = !this._search.regex;
      this._syncToggleButtons();
      this._scheduleSearch(0);
    });
    prevBtn.addEventListener('click', () => this._navMatch(-1));
    nextBtn.addEventListener('click', () => this._navMatch(1));
    replBtn.addEventListener('click', () => this._replaceCurrent());
    replAllBtn.addEventListener('click', () => this._replaceAll());
    closeBtn.addEventListener('click', () => this._closeSearch());
    bar.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        this._navMatch(e.shiftKey ? -1 : 1);
      } else if (e.key === 'Escape') {
        e.preventDefault();
        this._closeSearch();
      }
    });
    this._syncToggleButtons();
    this._syncReadonlyUi();
  }

  _applyFindbarLabels() {
    const l = this._labels;
    this._fbFind.setAttribute('placeholder', l.find);
    this._fbFind.setAttribute('aria-label', l.find);
    this._fbReplace.setAttribute('placeholder', l.replace);
    this._fbReplace.setAttribute('aria-label', l.replace);
    this._fbCase.setAttribute('aria-label', l.match_case);
    this._fbCase.setAttribute('title', l.match_case);
    this._fbRegex.setAttribute('aria-label', l.regex);
    this._fbRegex.setAttribute('title', l.regex);
    this._fbRepl.textContent = l.replace_one;
    this._fbReplAll.textContent = l.replace_all;
    const btns = this._findbar.querySelectorAll('tf-button');
    btns[2].setAttribute('aria-label', l.prev);
    btns[2].setAttribute('title', l.prev);
    btns[3].setAttribute('aria-label', l.next);
    btns[3].setAttribute('title', l.next);
    btns[6].setAttribute('aria-label', l.close);
    btns[6].setAttribute('title', l.close);
  }

  _syncToggleButtons() {
    this._fbCase.setAttribute('variant', this._search.caseSense ? 'primary' : 'ghost');
    this._fbRegex.setAttribute('variant', this._search.regex ? 'primary' : 'ghost');
  }

  _syncReadonlyUi() {
    if (!this._findbar) return;
    const ro = this.readOnly;
    this._fbReplace.style.display = ro ? 'none' : '';
    this._fbRepl.style.display = ro ? 'none' : '';
    this._fbReplAll.style.display = ro ? 'none' : '';
  }

  _closeSearch() {
    this._search.open = false;
    if (this._findbar) this._findbar.hidden = true;
    this._matches = [];
    this._activeMatch = -1;
    this._scheduleRender();
    this.focus();
  }

  _scheduleSearch(delay = 120) {
    clearTimeout(this._searchTimer);
    this._searchTimer = setTimeout(() => this._updateMatches(), delay);
  }

  _buildRegex(extraFlags = '') {
    const q = this._search.query;
    if (!q) return null;
    const source = this._search.regex ? q : q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const flags = 'g' + (this._search.caseSense ? '' : 'i') + extraFlags;
    return new RegExp(source, flags);
  }

  _updateMatches() {
    this._matches = [];
    this._activeMatch = -1;
    this._searchError = false;
    if (this._search.open && this._search.query) {
      let re;
      try {
        re = this._buildRegex();
      } catch {
        this._searchError = true;
        re = null;
      }
      if (re) {
        outer:
        for (let l = 0; l < this._lines.length; l++) {
          const text = this._lines[l];
          re.lastIndex = 0;
          let m;
          while ((m = re.exec(text)) !== null) {
            if (m[0] === '') { re.lastIndex++; continue; }
            this._matches.push({ line: l, start: m.index, end: m.index + m[0].length });
            if (this._matches.length >= MAX_MATCHES) break outer;
          }
        }
      }
      if (this._matches.length) {
        // pick the first match at/after the caret
        const head = this._head;
        let idx = this._matches.findIndex((mt) =>
          mt.line > head.line || (mt.line === head.line && mt.start >= head.ch));
        if (idx === -1) idx = 0;
        this._activeMatch = idx;
      }
    }
    this._updateSearchCount();
    this._scheduleRender();
  }

  _updateSearchCount() {
    if (!this._fbCount) return;
    const l = this._labels;
    this._fbCount.classList.toggle('err', this._searchError);
    if (this._searchError) this._fbCount.textContent = l.bad_regex;
    else if (!this._search.query) this._fbCount.textContent = '';
    else if (!this._matches.length) this._fbCount.textContent = l.no_matches;
    else this._fbCount.textContent = fmt(l.matches, { i: this._activeMatch + 1, n: this._matches.length });
  }

  _navMatch(dir) {
    if (!this._matches.length) return;
    this._activeMatch = (this._activeMatch + dir + this._matches.length) % this._matches.length;
    const mt = this._matches[this._activeMatch];
    this._unfoldAt(mt.line);
    this._anchor = { line: mt.line, ch: mt.start };
    this._head = { line: mt.line, ch: mt.end };
    this._updateBracket();
    this._revealPos(this._head, true);
    this._updateSearchCount();
    this._scheduleRender();
  }

  _replacementFor(mt) {
    const text = this._lines[mt.line].slice(mt.start, mt.end);
    if (!this._search.regex) return this._search.replace;
    try {
      const re = new RegExp('^(?:' + (this._search.query) + ')$', this._search.caseSense ? '' : 'i');
      return text.replace(re, this._search.replace);
    } catch {
      return this._search.replace;
    }
  }

  _replaceCurrent() {
    if (this.readOnly || this._activeMatch < 0) return;
    const mt = this._matches[this._activeMatch];
    this._unfoldAt(mt.line);
    this._edit({ line: mt.line, ch: mt.start }, { line: mt.line, ch: mt.end }, this._replacementFor(mt), 'replace');
    this._updateMatches();
    if (this._matches.length) this._navMatch(0);
  }

  _replaceAll() {
    if (this.readOnly || !this._matches.length) return;
    // bottom-up so earlier positions stay valid while applying
    const steps = [...this._matches].reverse().map((mt) => ({
      from: { line: mt.line, ch: mt.start },
      to: { line: mt.line, ch: mt.end },
      text: this._replacementFor(mt),
    }));
    for (const mt of this._matches) this._unfoldAt(mt.line);
    this._editGroup(steps, 'replace-all');
    this._updateMatches();
  }
}

customElements.define('tf-code-editor', TfCodeEditor);
