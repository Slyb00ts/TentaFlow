-- =============================================================================
-- go2 addon — robot connection + telemetry state. Single-row `robot` table holds
-- the live connection state machine, telemetry and the durable e-stop gate.
-- =============================================================================

CREATE TABLE IF NOT EXISTS robot (
    id           TEXT PRIMARY KEY,              -- stable, e.g. 'go2'
    ip           TEXT NOT NULL DEFAULT '',
    status       TEXT NOT NULL DEFAULT 'offline'
                 CHECK(status IN ('offline','connecting','validating','online','error')),
    status_msg   TEXT,
    channel_id   TEXT,                          -- host webrtc channel id
    camera_id    TEXT,                          -- backed camera id (when bound)
    battery_pct  INTEGER,                       -- BMS SOC from lowstate
    rtt_ms       INTEGER,                       -- keepalive RTT
    estop_active INTEGER NOT NULL DEFAULT 0     -- durable global safety gate
                 CHECK(estop_active IN (0,1)),
    last_update  INTEGER NOT NULL DEFAULT 0,
    tick_count   INTEGER NOT NULL DEFAULT 0
);
