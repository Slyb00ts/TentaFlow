// =============================================================================
// Plik: modules/flows-builder/node-visuals.js
// Opis: Jedno zrodlo ikony i koloru bloku Flow Buildera - uzywane przez palete
//       i canvas, zeby ten sam blok wygladal tak samo w obu miejscach.
// =============================================================================

// Backend nazywa ikony po swojemu (zestaw lucide), nasz sprite ma wlasne id.
// Bez tego mapowania kazdy blok spoza listy TYPE_ICON dostawal te sama szara
// ikone zastepcza, wiec paleta wygladala jak 70 kopii jednego wpisu.
const ICON_ALIAS = {
  hourglass: 'clock',
  timer: 'clock',
  grid: 'grid-2x2',
  layout: 'grid-rows',
  table: 'grid-rows',
  merge: 'branch',
  'git-branch': 'branch',
  repeat: 'rotate',
  activity: 'chart-line',
  hash: 'sparkle',
  'scan-text': 'file-text',
  'file-scan': 'image',
  'file-image': 'image',
  'book-open': 'folder',
  'volume-2': 'volume',
  wrench: 'services',
  'help-circle': 'question',
  'align-left': 'list',
  scissors: 'layers',
  eraser: 'transform',
  presentation: 'file',
  'message-circle': 'chat',
  'minimize-2': 'collapse',
};

// node_type -> ikona sprite. Trzymane dla blokow, ktorych ikona z backendu nie
// oddaje roli bloku w flow (np. Code Studio wykonuje komendy w terminalu).
const TYPE_ICON = {
  trigger: 'bolt', start: 'bolt',
  llm: 'brain', embeddings: 'sparkle', reranker: 'sparkle',
  stt: 'mic', tts: 'speaker',
  memory: 'database',
  conversation_history: 'chat', session_context: 'database',
  speaker_context: 'user', memory_analyzer: 'sparkle',
  condition: 'branch', switch: 'branch',
  template: 'code', transform: 'transform', router: 'transform',
  pii_filter: 'shield', tts_clean: 'shield',
  output: 'arrow-out', end: 'arrow-out',
  persist_turn: 'save',
  spawn: 'users', await_subagents: 'clock', subagent_status: 'chart-line', interval: 'clock',
  workspace_context: 'terminal', patch_review: 'eye',
  exec_command: 'terminal', delegate_cli: 'bot',
};

const CATEGORY_ICON = {
  trigger: 'bolt',
  service: 'chip',
  memory: 'database',
  transform: 'transform',
  logic: 'branch',
  filter: 'shield',
  output: 'arrow-out',
  other: 'puzzle',
};

// node_type -> zmienna koloru. Bloki bez wlasnego wpisu biora kolor kategorii,
// zeby rodzina (dokumenty, agenci, narzedzia) czytala sie jako jedna grupa.
const TYPE_VAR = {
  trigger: '--node-trigger', start: '--node-start', on_subagent_complete: '--node-trigger',
  llm: '--node-llm', stt: '--node-stt', tts: '--node-tts',
  memory: '--node-memory',
  embeddings: '--node-embeddings', reranker: '--node-reranker',
  condition: '--node-condition', switch: '--node-switch',
  template: '--node-template', transform: '--node-transform',
  pii_filter: '--node-pii_filter', tts_clean: '--node-tts_clean',
  router: '--node-router', output: '--node-output', end: '--node-end',
  conversation_history: '--node-conversation_history',
  session_context: '--node-session_context',
  speaker_context: '--node-speaker_context',
  memory_analyzer: '--node-memory_analyzer',
  persist_turn: '--node-conversation_history',
  spawn: '--node-spawn', await_subagents: '--node-spawn',
  subagent_status: '--node-spawn', interval: '--node-spawn',
  agent: '--node-agent', agent_context: '--node-agent', agent_router: '--node-agent',
  delegate_cli: '--node-agent',
  workspace_context: '--node-code_studio', patch_review: '--node-code_studio',
  exec_command: '--node-code_studio',
  ocr: '--node-document', ocr_pages: '--node-document',
  page_detect: '--node-document', page_detect_pages: '--node-document',
  vision_parse: '--node-document', vision_parse_pages: '--node-document',
  document_parse: '--node-document', document_router: '--node-document',
  document_merge: '--node-document', table_structure: '--node-document',
  graphic_elements: '--node-document', pdf_rasterize: '--node-document',
  excel_extract: '--node-transform', pptx_extract: '--node-transform',
  word_extract: '--node-transform', text_extract: '--node-transform',
  chunk: '--node-transform', compact_context: '--node-transform',
  sentence_buffer: '--node-transform',
  project_knowledge: '--node-rag', store: '--node-rag', embed_chunks: '--node-embeddings',
  tool_exec: '--node-tools', ask_user: '--node-tools',
};

const CATEGORY_VAR = {
  trigger: '--node-cat-trigger',
  service: '--node-cat-service',
  memory: '--node-cat-memory',
  transform: '--node-cat-transform',
  logic: '--node-cat-logic',
  filter: '--node-cat-filter',
  output: '--node-cat-output',
  other: '--node-cat-other',
};

function spriteHas(id) {
  return !!(id && document.getElementById(`i-${id}`));
}

/**
 * Ikona bloku: wlasne mapowanie typu, potem ikona z szablonu (po aliasie na
 * nasz sprite), a na koncu ikona kategorii. Nigdy nie zwraca id spoza sprite'a.
 */
export function nodeIconId(nodeType, templateIcon = '', category = '') {
  const own = TYPE_ICON[nodeType];
  if (spriteHas(own)) return own;
  const raw = String(templateIcon || '');
  const aliased = ICON_ALIAS[raw] || raw;
  if (spriteHas(aliased)) return aliased;
  const cat = CATEGORY_ICON[category];
  return spriteHas(cat) ? cat : 'puzzle';
}

/** Zmienna CSS koloru bloku (`--node-*`). */
export function nodeColorVar(nodeType, category = '') {
  return TYPE_VAR[nodeType] || CATEGORY_VAR[category] || '--node-cat-other';
}
