-- ===== File: 001_init.sql — Translator addon schema =====
-- Optional local translation history, gated by the "save history" setting. The
-- live captioning mode is single-user and keeps its rolling on-screen lines in
-- panel state / addon storage, so it needs no tables here.

CREATE TABLE IF NOT EXISTS translation_history (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  src_lang TEXT NOT NULL,
  tgt_lang TEXT NOT NULL,
  source_text TEXT NOT NULL,
  target_text TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
