-- =============================================================================
-- File: addons-pro/tentavision/migrations/0001_init.sql
-- TentaVision full domain schema (SQLite). Source of truth for every panel:
-- cameras, profiles, alarms, zones, models, audit log, settings, evidence.
-- Applied per-instance by the host migration runner; DDL never runs at runtime.
-- =============================================================================

-- Cameras configured in this addon. status/fps reflect the last known runtime
-- snapshot; detectors is a comma-separated list of detector tokens.
CREATE TABLE IF NOT EXISTS cameras (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    location    TEXT NOT NULL DEFAULT '',
    rtsp_url    TEXT NOT NULL DEFAULT '',
    onvif_url   TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'offline',
    fps         INTEGER NOT NULL DEFAULT 0,
    detectors   TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL DEFAULT 0
);

-- Analytic profiles binding a flow + risk class + schedule to a set of cameras.
-- cameras is a JSON array of camera ids; schedule is a JSON blob.
CREATE TABLE IF NOT EXISTS profiles (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    flow_id     TEXT NOT NULL DEFAULT '',
    risk_class  TEXT NOT NULL DEFAULT '',
    schedule    TEXT NOT NULL DEFAULT '',
    cameras     TEXT NOT NULL DEFAULT '[]',
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL DEFAULT 0
);

-- Alarm events raised by analytics. status tracks the decision lifecycle
-- (new / acknowledged / dismissed); decided_by/decided_at record the operator.
CREATE TABLE IF NOT EXISTS alarms (
    id          TEXT PRIMARY KEY,
    camera_id   TEXT NOT NULL DEFAULT '',
    severity    TEXT NOT NULL DEFAULT 'info',
    type        TEXT NOT NULL DEFAULT '',
    message     TEXT NOT NULL DEFAULT '',
    thumb_ref   TEXT NOT NULL DEFAULT '',
    ts          INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL DEFAULT 'new',
    decided_by  TEXT NOT NULL DEFAULT '',
    decided_at  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_alarms_camera ON alarms(camera_id);
CREATE INDEX IF NOT EXISTS idx_alarms_ts ON alarms(ts);

-- Detection zones drawn on a camera. polygon is a JSON array of [x,y] points.
CREATE TABLE IF NOT EXISTS zones (
    id          TEXT PRIMARY KEY,
    camera_id   TEXT NOT NULL DEFAULT '',
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL DEFAULT 'detection',
    polygon     TEXT NOT NULL DEFAULT '[]',
    created_at  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_zones_camera ON zones(camera_id);

-- Inference models registered for analytics. vram_mb is the resident footprint.
CREATE TABLE IF NOT EXISTS models (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    runtime     TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT '',
    vram_mb     INTEGER NOT NULL DEFAULT 0,
    version     TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL DEFAULT 0
);

-- Append-only audit log with a hash chain (hash links to prev_hash) so the
-- record is tamper-evident. before/after carry JSON snapshots of the change.
CREATE TABLE IF NOT EXISTS audit_log (
    id          TEXT PRIMARY KEY,
    ts          INTEGER NOT NULL DEFAULT 0,
    actor       TEXT NOT NULL DEFAULT '',
    action      TEXT NOT NULL DEFAULT '',
    target      TEXT NOT NULL DEFAULT '',
    before      TEXT NOT NULL DEFAULT '',
    after       TEXT NOT NULL DEFAULT '',
    hash        TEXT NOT NULL DEFAULT '',
    prev_hash   TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);

-- Key/value settings for the addon (general, retention, notifications, ...).
CREATE TABLE IF NOT EXISTS settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL DEFAULT '',
    updated_at  INTEGER NOT NULL DEFAULT 0
);

-- Signed evidence packages exported for an alarm (for legal / GDPR export).
CREATE TABLE IF NOT EXISTS evidence (
    id          TEXT PRIMARY KEY,
    alarm_id    TEXT NOT NULL DEFAULT '',
    package_ref TEXT NOT NULL DEFAULT '',
    signed_by   TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_evidence_alarm ON evidence(alarm_id);
