// =============================================================================
// File: run-timeline-records.js — fixture for <tf-run-timeline>
// Description: the Z01 prototype dataset (mockups/zdarzenia-20260819) in the
//   component's record shape, plus one record still IN FLIGHT (duration null,
//   r27) and one that failed (r26). Fixture only — the browser reads the real
//   log over the binary protocol; nothing here is wired to a backend.
// =============================================================================

// Wall-clock instant of start = 0, so the axis can show a real clock instead of
// elapsed time.
export const RUN_TIMELINE_EPOCH = 1755600000000;

export const RUN_TIMELINE_RECORDS = [
  { id: 'r1', seq: 1, start: 0, duration: 4420, lane: 'model', kind: 'request', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'qwen3-27b', detail: 'system + 6 messages · 3 tools', turn: 1, ttft: 1414, error: false },
  { id: 'r2', seq: 2, start: 1414, duration: 40, lane: 'messages', kind: 'first_token', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: '—', detail: 'TTFT', turn: 1, ttft: null, error: false },
  { id: 'r3', seq: 3, start: 2431, duration: 38, lane: 'tools', kind: 'tool', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'core.fs_read', detail: 'src/db/seed.rs · call c-a70', turn: 1, ttft: null, error: false },
  { id: 'r4', seq: 4, start: 2461, duration: 60, lane: 'tools', kind: 'tool', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'core.fs_grep', detail: 'src/db/seed.rs · call c-a71', turn: 1, ttft: null, error: false },
  { id: 'r5', seq: 5, start: 5320, duration: 5040, lane: 'model', kind: 'request', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'qwen3-27b', detail: 'system + 9 messages · 3 tools', turn: 2, ttft: 1612, error: false },
  { id: 'r6', seq: 6, start: 6932, duration: 40, lane: 'messages', kind: 'first_token', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: '—', detail: 'TTFT', turn: 2, ttft: null, error: false },
  { id: 'r7', seq: 7, start: 8092, duration: 38, lane: 'tools', kind: 'tool', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'core.fs_read', detail: 'src/db/seed.rs · call c-a70', turn: 2, ttft: null, error: false },
  { id: 'r8', seq: 8, start: 8948, duration: 22, lane: 'tools', kind: 'tool', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'core.fs_write', detail: 'src/db/seed.rs · +38 −6', turn: 2, ttft: null, error: false },
  { id: 'r9', seq: 9, start: 11260, duration: 5660, lane: 'model', kind: 'request', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'qwen3-27b', detail: 'system + 12 messages · 3 tools', turn: 3, ttft: 1811, error: false },
  { id: 'r10', seq: 10, start: 13071, duration: 40, lane: 'messages', kind: 'first_token', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: '—', detail: 'TTFT', turn: 3, ttft: null, error: false },
  { id: 'r11', seq: 11, start: 14373, duration: 38, lane: 'tools', kind: 'tool', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'core.fs_read', detail: 'src/db/seed.rs · call c-a70', turn: 3, ttft: null, error: false },
  { id: 'r12', seq: 12, start: 14656, duration: 252000, lane: 'tools', kind: 'tool', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'core.exec', detail: 'cargo test --lib db::seed · 28 passed', turn: 3, ttft: null, error: false },
  { id: 'r13', seq: 13, start: 17820, duration: 2100, lane: 'model', kind: 'request', origin: 'project', actor: 'marek.nowak', actorKind: 'user', name: 'qwen3-27b', detail: 'retrieval multi-hop · hop 1/3', turn: 4, ttft: 520, error: false },
  { id: 'r14', seq: 14, start: 18120, duration: 84, lane: 'tools', kind: 'tool', origin: 'project', actor: 'marek.nowak', actorKind: 'user', name: 'vector', detail: 'passages · ps-4471 · 6 hits', turn: 4, ttft: null, error: false },
  { id: 'r15', seq: 15, start: 18240, duration: 1, lane: 'tools', kind: 'tool', origin: 'project', actor: 'marek.nowak', actorKind: 'user', name: 'reranker', detail: 'alias unbound → vector order', turn: 4, ttft: null, error: false },
  { id: 'r16', seq: 16, start: 20220, duration: 2100, lane: 'model', kind: 'request', origin: 'project', actor: 'marek.nowak', actorKind: 'user', name: 'qwen3-27b', detail: 'retrieval multi-hop · hop 2/3', turn: 4, ttft: 520, error: false },
  { id: 'r17', seq: 17, start: 20520, duration: 84, lane: 'tools', kind: 'tool', origin: 'project', actor: 'marek.nowak', actorKind: 'user', name: 'vector', detail: 'passages · ps-4471 · 6 hits', turn: 4, ttft: null, error: false },
  { id: 'r18', seq: 18, start: 20640, duration: 1, lane: 'tools', kind: 'tool', origin: 'project', actor: 'marek.nowak', actorKind: 'user', name: 'reranker', detail: 'alias unbound → vector order', turn: 4, ttft: null, error: false },
  { id: 'r19', seq: 19, start: 22620, duration: 2100, lane: 'model', kind: 'request', origin: 'project', actor: 'marek.nowak', actorKind: 'user', name: 'qwen3-27b', detail: 'retrieval multi-hop · hop 3/3', turn: 4, ttft: 520, error: false },
  { id: 'r20', seq: 20, start: 22920, duration: 84, lane: 'tools', kind: 'tool', origin: 'project', actor: 'marek.nowak', actorKind: 'user', name: 'vector', detail: 'passages · ps-4471 · 6 hits', turn: 4, ttft: null, error: false },
  { id: 'r21', seq: 21, start: 23040, duration: 1, lane: 'tools', kind: 'tool', origin: 'project', actor: 'marek.nowak', actorKind: 'user', name: 'reranker', detail: 'alias unbound → vector order', turn: 4, ttft: null, error: false },
  { id: 'r22', seq: 22, start: 25020, duration: 1118, lane: 'model', kind: 'request', origin: 'api', actor: 'billing-etl', actorKind: 'api_key', name: 'gpt-oss-120b', detail: 'chat/completions · 1 message', turn: 5, ttft: 310, error: false },
  { id: 'r23', seq: 23, start: 26520, duration: 1840, lane: 'model', kind: 'request', origin: 'api', actor: 'portal-asystent', actorKind: 'api_key', name: 'qwen3-27b', detail: 'chat/completions · 6 messages', turn: 6, ttft: 640, error: false },
  { id: 'r24', seq: 24, start: 28720, duration: 940, lane: 'model', kind: 'request', origin: 'addon', actor: 'rag', actorKind: 'addon', name: 'rag-llm', detail: 'retrieval judge · instance rag-7a1', turn: 7, ttft: 250, error: false },
  { id: 'r25', seq: 25, start: 29920, duration: 3100, lane: 'model', kind: 'request', origin: 'chat', actor: 'anna.kowalska', actorKind: 'user', name: 'qwen3-27b', detail: 'dashboard chat', turn: 8, ttft: 900, error: false },
  { id: 'r26', seq: 26, start: 31120, duration: 900, lane: 'tools', kind: 'tool', origin: 'chat', actor: 'anna.kowalska', actorKind: 'user', name: 'notes.search', detail: '120 s budget exceeded → TOOL_TIMEOUT', turn: 8, ttft: null, error: true },
  { id: 'r27', seq: 27, start: 33020, duration: null, lane: 'model', kind: 'request', origin: 'code_studio', actor: 'anna.kowalska', actorKind: 'user', name: 'qwen3-27b', detail: 'system + 15 messages · 3 tools', turn: 9, ttft: null, error: false },
];
