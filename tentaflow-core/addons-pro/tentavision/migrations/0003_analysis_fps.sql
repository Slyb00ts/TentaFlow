-- =============================================================================
-- 0003_analysis_fps.sql
-- Per-camera AI analysis frame rate honored by the always-on analysis loop.
-- 0 = unlimited (native cadence); default 10 matches the core spec default.
-- Mirrors the core `cameras.analysis_fps` column so the addon row carries the
-- operator's choice and can re-supply it on re-registration ("Odśwież status").
-- =============================================================================

ALTER TABLE cameras ADD COLUMN analysis_fps INTEGER NOT NULL DEFAULT 10;
