-- =============================================================================
-- go2 addon — cross-worker live stream state. The DB+instance concurrency
-- overhaul runs tool calls on ephemeral pooled workers that SKIP the service
-- instance, so live telemetry/lidar parsed by the service instance's on_tick
-- can no longer live in thread_local memory (a status worker would see nothing).
-- This single-row table is the source of truth any worker can read: the service
-- instance writes the latest snapshots, every worker (go2.status, lidar_frame,
-- lidar_on/off) reads/writes it.
-- =============================================================================

CREATE TABLE IF NOT EXISTS robot_live (
    id              TEXT PRIMARY KEY,             -- mirrors robot.id, e.g. 'go2'
    -- Latest structured telemetry, stored as the EXACT JSON object that
    -- telemetry_json() builds (and parse_status_telemetry consumes). Empty
    -- string means "nothing received yet" → go2.status omits the key.
    telemetry_json  TEXT NOT NULL DEFAULT '',
    telemetry_ts    INTEGER NOT NULL DEFAULT 0,   -- last telemetry persist (throttle gate)
    -- Operator's persistent LiDAR enable INTENT (desired state), written by
    -- lidar_on/off on ANY worker and read by the service instance's on_tick.
    lidar_enabled   INTEGER NOT NULL DEFAULT 0
                    CHECK(lidar_enabled IN (0,1)),
    -- SMALL LiDAR status (availability metadata, NEVER the point cloud), stored
    -- as the EXACT JSON object lidar_status_json() builds. Empty string means no
    -- frame decoded this session.
    lidar_status_json TEXT NOT NULL DEFAULT ''
);
