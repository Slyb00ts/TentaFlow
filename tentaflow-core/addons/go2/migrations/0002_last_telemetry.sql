-- =============================================================================
-- go2 addon — add last_telemetry: timestamp of the last REAL lowstate receipt.
-- Drives the online liveness watchdog (distinct from last_update, which tracks
-- any state transition). Separate migration so existing installs upgrade cleanly.
-- =============================================================================

ALTER TABLE robot ADD COLUMN last_telemetry INTEGER NOT NULL DEFAULT 0;
