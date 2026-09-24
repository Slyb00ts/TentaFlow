// =============================================================================
// File: tentanas/db.rs — schema and row access of the per-node `tentanas.db`
//       (plan-02 §5.2). The file lives in the instance data dir and is opened
//       through `addon::app_db`; nothing in it is synced — every node keeps
//       its own disks, samples, alerts and jobs, the dashboard reaches them
//       through node forwarding. Settings that must reach other nodes go to
//       `addon_config` of the instance instead.
// =============================================================================

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use tentaflow_protocol::tentanas::{
    NasAccessEvent, NasAlert, NasDiskSample, NasHealthReason, NasJob, NasNfsOptions, NasSchedule,
    NasShareAccess, NasShareUser, NasSmartSchedule, NasSmbOptions, NasSnapshotSchedule, NasTargetLun,
    NasTargetPortGroup, NasTargetPortal,
};

use crate::db::DbPool;
use std::collections::BTreeMap;
use tentanas_helper::elastic::{ElasticClaim, ElasticCreateSpec, ElasticDiskSpec, ElasticOwner, ElasticResult};
use super::jobs::ElasticJobIntent;

const APP: &str = "tentanas";

/// The scrub cadence every Elastic Array with parity is given (owner decision
/// 2026-09-22), spelled ONCE as the JSON `nas_elastic_schedules.schedule_json`
/// holds, because two writers need it and one of them is SQL: migration 19
/// backfills it and `insert_default_scrub_schedule` writes it for a new or
/// adopted array. A macro and not a `const`, so `concat!` can put it inside
/// the migration's string literal; `default_elastic_scrub_schedule` is the
/// same value as a struct, and a test pins the two to each other byte for
/// byte (`serde_json` writes the fields in declaration order).
macro_rules! default_elastic_scrub_schedule_json {
    () => {
        r#"{"every":"monthly","hour":4,"minute":0,"weekday":0,"day":15}"#
    };
}

/// Append-only. A released step is never edited: the runner records applied
/// versions per file and only executes the ones above the recorded maximum.
const MIGRATIONS: &[(i64, &str)] = &[(
    1,
    "CREATE TABLE nas_settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE nas_environment (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT NOT NULL,
        probed_at TEXT NOT NULL
    );
    CREATE TABLE nas_disks (
        disk_id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        model TEXT NOT NULL,
        serial TEXT NOT NULL,
        wwn TEXT,
        size_bytes INTEGER NOT NULL,
        kind TEXT NOT NULL,
        first_seen_at TEXT NOT NULL,
        last_seen_at TEXT NOT NULL,
        smart_json TEXT,
        smart_read_at TEXT,
        health TEXT NOT NULL DEFAULT 'unknown',
        health_reason TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE nas_disk_samples (
        disk_id TEXT NOT NULL,
        at TEXT NOT NULL,
        temperature_c INTEGER,
        reallocated INTEGER,
        pending INTEGER,
        crc_errors INTEGER,
        media_errors INTEGER,
        read_bps INTEGER NOT NULL DEFAULT 0,
        write_bps INTEGER NOT NULL DEFAULT 0,
        await_ms REAL NOT NULL DEFAULT 0,
        PRIMARY KEY (disk_id, at)
    ) WITHOUT ROWID;
    CREATE TABLE nas_alerts (
        alert_id TEXT PRIMARY KEY,
        severity TEXT NOT NULL,
        subject_kind TEXT NOT NULL,
        subject_id TEXT NOT NULL,
        title TEXT NOT NULL,
        detail TEXT NOT NULL,
        raised_at TEXT NOT NULL,
        acked_at TEXT,
        resolved_at TEXT,
        dedupe_key TEXT NOT NULL
    );
    CREATE UNIQUE INDEX nas_alerts_open_dedupe
        ON nas_alerts(dedupe_key) WHERE resolved_at IS NULL;
    CREATE INDEX nas_alerts_subject ON nas_alerts(subject_kind, subject_id);
    CREATE TABLE nas_jobs (
        job_id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        subject TEXT NOT NULL,
        status TEXT NOT NULL,
        progress_pct INTEGER,
        started_by TEXT NOT NULL,
        started_at TEXT NOT NULL,
        finished_at TEXT,
        error TEXT,
        log TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX nas_jobs_started ON nas_jobs(started_at DESC);",
), (
    2,
    // Recurring work of the node and the pool throughput history. Nothing here
    // mirrors ZFS state: a schedule is an intention, and `zpool`/`zfs` stay the
    // only source of truth for what actually exists.
    "CREATE TABLE nas_scrub_schedules (
        pool TEXT PRIMARY KEY,
        enabled INTEGER NOT NULL DEFAULT 0,
        schedule_json TEXT NOT NULL,
        last_run_at TEXT,
        last_result TEXT NOT NULL DEFAULT '',
        next_run_at TEXT
    );
    CREATE TABLE nas_snapshot_schedules (
        schedule_id TEXT PRIMARY KEY,
        dataset TEXT NOT NULL UNIQUE,
        enabled INTEGER NOT NULL DEFAULT 0,
        recursive INTEGER NOT NULL DEFAULT 0,
        schedule_json TEXT NOT NULL,
        keep_frequent INTEGER NOT NULL DEFAULT 0,
        keep_hourly INTEGER NOT NULL DEFAULT 0,
        keep_daily INTEGER NOT NULL DEFAULT 0,
        keep_weekly INTEGER NOT NULL DEFAULT 0,
        keep_monthly INTEGER NOT NULL DEFAULT 0,
        last_run_at TEXT,
        last_result TEXT NOT NULL DEFAULT '',
        next_run_at TEXT
    );
    CREATE TABLE nas_pool_samples (
        pool TEXT NOT NULL,
        sampled_at TEXT NOT NULL,
        read_bps INTEGER NOT NULL DEFAULT 0,
        write_bps INTEGER NOT NULL DEFAULT 0,
        read_iops REAL NOT NULL DEFAULT 0,
        write_iops REAL NOT NULL DEFAULT 0,
        read_latency_ms REAL NOT NULL DEFAULT 0,
        write_latency_ms REAL NOT NULL DEFAULT 0,
        PRIMARY KEY (pool, sampled_at)
    ) WITHOUT ROWID;",
), (
    3,
    // File shares and the local Samba accounts they grant to. This is the
    // DESIRED state: `/etc/samba/tentanas.conf` and the exports file are
    // generated from these rows on every change, never read back — one source
    // of truth, no drift (§3.4 "Persystencja po reboocie"). Passwords are not
    // here and never were: the helper hands them to `smbpasswd` and forgets.
    "CREATE TABLE nas_shares (
        share_id TEXT PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        protocol TEXT NOT NULL,
        source_path TEXT NOT NULL,
        dataset TEXT,
        enabled INTEGER NOT NULL DEFAULT 1,
        fleet_mount INTEGER NOT NULL DEFAULT 1,
        options_json TEXT NOT NULL DEFAULT '{}',
        state TEXT NOT NULL DEFAULT 'disabled',
        state_detail TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE nas_share_users (
        name TEXT PRIMARY KEY,
        description TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL
    );
    CREATE TABLE nas_share_grants (
        share_id TEXT NOT NULL,
        user TEXT NOT NULL,
        mode TEXT NOT NULL,
        PRIMARY KEY (share_id, user)
    ) WITHOUT ROWID;
    CREATE INDEX nas_share_grants_user ON nas_share_grants(user);",
), (
    4,
    // The 30-day half of the disk history (§5.4, the charts labelled "30 dni").
    // Same columns as the minute table so one reader can concatenate both:
    // `nas_disk_samples` holds the last 48 h at minute resolution, this one
    // holds hourly rows for 30 days. Keeping 30 days of minutes instead would
    // be ~43k rows per disk for a chart that cannot draw them.
    "CREATE TABLE nas_disk_hourly (
        disk_id TEXT NOT NULL,
        at TEXT NOT NULL,
        temperature_c INTEGER,
        reallocated INTEGER,
        pending INTEGER,
        crc_errors INTEGER,
        media_errors INTEGER,
        read_bps INTEGER NOT NULL DEFAULT 0,
        write_bps INTEGER NOT NULL DEFAULT 0,
        await_ms REAL NOT NULL DEFAULT 0,
        PRIMARY KEY (disk_id, at)
    ) WITHOUT ROWID;",
), (
    5,
    // Protected snapshots (§5.10). ZFS holds carry no expiry, so the period
    // the admin asked for is an INTENTION only this table knows; ZFS stays the
    // truth about whether the hold is still there. Rows are joined against the
    // live snapshot list, so one left behind by a snapshot that finally went
    // away is invisible rather than wrong.
    "ALTER TABLE nas_snapshot_schedules ADD COLUMN protect_days INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE nas_snapshot_protection (
        snapshot TEXT PRIMARY KEY,
        protect_days INTEGER NOT NULL,
        protected_until TEXT NOT NULL,
        protected_by TEXT NOT NULL,
        protected_at TEXT NOT NULL
    );",
), (
    6,
    // Red-path operations parked for a second admin (§5.10). `payload_json` is
    // the request as it arrived, MINUS its sudo password: a password never
    // reaches disk (§3.4), so the approving admin supplies their own. `org_id`
    // and `addon_id` ride along because the expiry sweep runs in the scheduler
    // loop, which has no request context to audit against.
    //
    // `recursive` on the protection record is what an approved release needs:
    // a hold placed with `-r` only comes off with `-r`, and ZFS does not say
    // which one was used.
    "ALTER TABLE nas_snapshot_protection ADD COLUMN recursive INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE nas_pending_approvals (
        request_id TEXT PRIMARY KEY,
        operation TEXT NOT NULL,
        subject TEXT NOT NULL,
        detail TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        status TEXT NOT NULL,
        org_id TEXT NOT NULL,
        addon_id TEXT NOT NULL,
        requested_by TEXT NOT NULL,
        requested_at TEXT NOT NULL,
        expires_at TEXT NOT NULL,
        decided_by TEXT,
        decided_at TEXT,
        decision_note TEXT NOT NULL DEFAULT '',
        decision_job_id TEXT
    );
    CREATE INDEX nas_pending_approvals_open ON nas_pending_approvals(status, expires_at);",
), (
    7,
    // The file access audit and the two cheap extras of §5.10.
    //
    // `nas_access_events` holds what `vfs_full_audit` wrote to syslog, parsed.
    // It is a LOG, not a mirror of anything: rows are append-only and pruned by
    // age, and `journal_cursor` in nas_settings is where the last collection
    // stopped, so a restart neither re-reads nor skips a line. `forwarded_at`
    // on both this table and `nas_alerts` is the forwarder's own bookkeeping
    // (§5.9): one column instead of a queue table, because the rows already
    // are the queue and forwarding twice is worse than forwarding late.
    //
    // `nas_trim_schedules` is the scrub schedule's table shape exactly — same
    // columns, same reader — because a recurring pool task is the same row
    // whichever verb it runs.
    "CREATE TABLE nas_access_events (
        event_id INTEGER PRIMARY KEY AUTOINCREMENT,
        at TEXT NOT NULL,
        share TEXT NOT NULL,
        user TEXT NOT NULL,
        client TEXT NOT NULL,
        operation TEXT NOT NULL,
        result TEXT NOT NULL,
        target TEXT NOT NULL,
        detail TEXT NOT NULL DEFAULT '',
        forwarded_at TEXT
    );
    CREATE INDEX nas_access_events_at ON nas_access_events(at DESC);
    CREATE INDEX nas_access_events_share ON nas_access_events(share, at DESC);
    CREATE INDEX nas_access_events_pending ON nas_access_events(forwarded_at) WHERE forwarded_at IS NULL;
    ALTER TABLE nas_alerts ADD COLUMN forwarded_at TEXT;
    CREATE TABLE nas_trim_schedules (
        pool TEXT PRIMARY KEY,
        enabled INTEGER NOT NULL DEFAULT 0,
        schedule_json TEXT NOT NULL,
        last_run_at TEXT,
        last_result TEXT NOT NULL DEFAULT '',
        next_run_at TEXT
    );",
), (
    8,
    // Block targets (§5.5). The DESIRED state, exactly like `nas_shares`: the
    // node writes LIO's and nvmet's configfs from these rows and never reads
    // them back, and it writes them again when the instance starts, because
    // configfs is EMPTY after a reboot. That is the whole reason there is no
    // `targetcli saveconfig` and no `target.service` anywhere in this app —
    // one source of truth (§3.4 "Persystencja po reboocie").
    //
    // `wwn` is the IQN or NQN clients connect to. UNIQUE because it is also a
    // configfs directory name: two rows claiming one would fight over the
    // same kernel object.
    //
    // `spec_json` carries the LUNs, portals and port groups — structure the
    // relational shape would only spread over three more tables that are
    // always read and written together. The INITIATOR allowlist is its own
    // table for the same reason the share grants are: dropping one initiator
    // everywhere is then one statement.
    //
    // The four CHAP / DH-HMAC-CHAP columns hold `SettingsCipher` ciphertext
    // BOUND to the target id, never a plaintext secret: a row copied into
    // another target's id fails to decrypt instead of authenticating there.
    "CREATE TABLE nas_targets (
        target_id TEXT PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        protocol TEXT NOT NULL,
        wwn TEXT NOT NULL UNIQUE,
        enabled INTEGER NOT NULL DEFAULT 1,
        spec_json TEXT NOT NULL DEFAULT '{}',
        auth_method TEXT NOT NULL DEFAULT 'none',
        auth_username TEXT NOT NULL DEFAULT '',
        auth_secret TEXT NOT NULL DEFAULT '',
        auth_mutual_username TEXT NOT NULL DEFAULT '',
        auth_mutual_secret TEXT NOT NULL DEFAULT '',
        dhchap_hash TEXT NOT NULL DEFAULT '',
        dhchap_dhgroup TEXT NOT NULL DEFAULT '',
        state TEXT NOT NULL DEFAULT 'disabled',
        state_detail TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE nas_target_initiators (
        target_id TEXT NOT NULL,
        initiator TEXT NOT NULL,
        PRIMARY KEY (target_id, initiator)
    ) WITHOUT ROWID;
    CREATE INDEX nas_target_initiators_name ON nas_target_initiators(initiator);",
), (
    9,
    "CREATE TABLE nas_elastic_arrays (
        array_id TEXT PRIMARY KEY,
        org_id TEXT NOT NULL,
        addon_id TEXT NOT NULL,
        name TEXT NOT NULL UNIQUE,
        filesystem TEXT NOT NULL CHECK(filesystem IN ('xfs','ext4')),
        state TEXT NOT NULL CHECK(state IN ('creating','active','needs_attention')),
        state_detail TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE nas_elastic_disks (
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        role TEXT NOT NULL CHECK(role IN ('data','parity')),
        slot INTEGER NOT NULL CHECK(slot > 0 AND (role != 'parity' OR slot <= 2)),
        disk_id TEXT NOT NULL UNIQUE,
        wwn TEXT CHECK(wwn IS NULL OR length(wwn) > 0),
        serial TEXT CHECK(serial IS NULL OR length(serial) > 0),
        bytes INTEGER NOT NULL CHECK(bytes > 0),
        expected_uuid TEXT NOT NULL UNIQUE,
        CHECK(wwn IS NOT NULL OR serial IS NOT NULL),
        PRIMARY KEY(array_id, role, slot)
    );
    CREATE TABLE nas_elastic_disk_aliases (
        kind TEXT NOT NULL CHECK(kind IN ('disk_id','wwn','serial')),
        value TEXT NOT NULL CHECK(length(value) > 0),
        array_id TEXT NOT NULL,
        role TEXT NOT NULL,
        slot INTEGER NOT NULL,
        PRIMARY KEY(kind, value),
        FOREIGN KEY(array_id, role, slot)
            REFERENCES nas_elastic_disks(array_id, role, slot) ON DELETE RESTRICT
    );
    CREATE INDEX nas_elastic_alias_disk ON nas_elastic_disk_aliases(array_id, role, slot);
    CREATE TABLE nas_elastic_operations (
        operation_id TEXT PRIMARY KEY,
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        job_id TEXT NOT NULL UNIQUE REFERENCES nas_jobs(job_id) ON DELETE RESTRICT,
        kind TEXT NOT NULL CHECK(kind IN ('create','restore')),
        state TEXT NOT NULL CHECK(state IN ('running','succeeded','needs_attention')),
        request_json TEXT NOT NULL,
        result_json TEXT,
        error TEXT NOT NULL,
        created_at TEXT NOT NULL,
        finished_at TEXT
    );
    CREATE INDEX nas_elastic_operation_array ON nas_elastic_operations(array_id, created_at);
    CREATE UNIQUE INDEX nas_elastic_operation_running
        ON nas_elastic_operations(array_id) WHERE state = 'running';
    CREATE UNIQUE INDEX nas_elastic_operation_create
        ON nas_elastic_operations(array_id) WHERE kind = 'create';",
), (
    10,
    "CREATE TABLE nas_elastic_operations_new (
        operation_id TEXT PRIMARY KEY,
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        job_id TEXT NOT NULL UNIQUE REFERENCES nas_jobs(job_id) ON DELETE RESTRICT,
        kind TEXT NOT NULL CHECK(kind IN ('create','restore','sync','scrub')),
        state TEXT NOT NULL CHECK(state IN ('running','succeeded','failed','needs_attention')),
        request_json TEXT NOT NULL,
        result_json TEXT,
        error TEXT NOT NULL,
        created_at TEXT NOT NULL,
        finished_at TEXT
    );
    INSERT INTO nas_elastic_operations_new
        SELECT operation_id,array_id,job_id,kind,state,request_json,result_json,error,created_at,finished_at
        FROM nas_elastic_operations;
    DROP TABLE nas_elastic_operations;
    ALTER TABLE nas_elastic_operations_new RENAME TO nas_elastic_operations;
    CREATE INDEX nas_elastic_operation_array ON nas_elastic_operations(array_id, created_at);
    CREATE UNIQUE INDEX nas_elastic_operation_running
        ON nas_elastic_operations(array_id) WHERE state = 'running';
    CREATE UNIQUE INDEX nas_elastic_operation_create
        ON nas_elastic_operations(array_id) WHERE kind = 'create';",
), (
    11,
    "CREATE TABLE nas_elastic_disks_new (
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        role TEXT NOT NULL CHECK(role IN ('data','cache','parity')),
        slot INTEGER NOT NULL CHECK(slot > 0 AND ((role = 'cache' AND slot = 1) OR (role = 'parity' AND slot <= 2) OR role = 'data')),
        disk_id TEXT NOT NULL UNIQUE,
        wwn TEXT CHECK(wwn IS NULL OR length(wwn) > 0),
        serial TEXT CHECK(serial IS NULL OR length(serial) > 0),
        bytes INTEGER NOT NULL CHECK(bytes > 0),
        expected_uuid TEXT NOT NULL UNIQUE,
        CHECK(wwn IS NOT NULL OR serial IS NOT NULL),
        PRIMARY KEY(array_id, role, slot)
    );
    INSERT INTO nas_elastic_disks_new SELECT array_id,role,slot,disk_id,wwn,serial,bytes,expected_uuid FROM nas_elastic_disks;
    CREATE TABLE nas_elastic_disk_aliases_new (
        kind TEXT NOT NULL CHECK(kind IN ('disk_id','wwn','serial')),
        value TEXT NOT NULL CHECK(length(value) > 0),
        array_id TEXT NOT NULL,
        role TEXT NOT NULL,
        slot INTEGER NOT NULL,
        PRIMARY KEY(kind, value),
        FOREIGN KEY(array_id, role, slot) REFERENCES nas_elastic_disks_new(array_id, role, slot) ON DELETE RESTRICT
    );
    INSERT INTO nas_elastic_disk_aliases_new SELECT kind,value,array_id,role,slot FROM nas_elastic_disk_aliases;
    DROP TABLE nas_elastic_disk_aliases;
    DROP TABLE nas_elastic_disks;
    ALTER TABLE nas_elastic_disks_new RENAME TO nas_elastic_disks;
    ALTER TABLE nas_elastic_disk_aliases_new RENAME TO nas_elastic_disk_aliases;
    CREATE INDEX nas_elastic_alias_disk ON nas_elastic_disk_aliases(array_id, role, slot);",
), (
    12,
    // The mover becomes an operation kind of its own (E2-09). It is a rebuild
    // and not an ALTER because the kind is a CHECK constraint, and SQLite has
    // no way to widen one in place.
    "CREATE TABLE nas_elastic_operations_new (
        operation_id TEXT PRIMARY KEY,
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        job_id TEXT NOT NULL UNIQUE REFERENCES nas_jobs(job_id) ON DELETE RESTRICT,
        kind TEXT NOT NULL CHECK(kind IN ('create','restore','sync','scrub','mover')),
        state TEXT NOT NULL CHECK(state IN ('running','succeeded','failed','needs_attention')),
        request_json TEXT NOT NULL,
        result_json TEXT,
        error TEXT NOT NULL,
        created_at TEXT NOT NULL,
        finished_at TEXT
    );
    INSERT INTO nas_elastic_operations_new
        SELECT operation_id,array_id,job_id,kind,state,request_json,result_json,error,created_at,finished_at
        FROM nas_elastic_operations;
    DROP TABLE nas_elastic_operations;
    ALTER TABLE nas_elastic_operations_new RENAME TO nas_elastic_operations;
    CREATE INDEX nas_elastic_operation_array ON nas_elastic_operations(array_id, created_at);
    CREATE UNIQUE INDEX nas_elastic_operation_running
        ON nas_elastic_operations(array_id) WHERE state = 'running';
    CREATE UNIQUE INDEX nas_elastic_operation_create
        ON nas_elastic_operations(array_id) WHERE kind = 'create';",
), (
    13,
    // Where an Elastic schedule lives (E2-10). Until now `MoverConfig` and
    // `SnapraidConfig` were built from defaults on every read, so the mover
    // cadence and the two SnapRAID cadences were placeholders on the wire and
    // nothing could ever fire them.
    //
    // ONE table keyed `(array_id, kind)` rather than three: the three cadences
    // have the same row shape as each other and as `nas_scrub_schedules`, so
    // one shape means one reader, one writer and one loop over the kinds. The
    // kind is a bound parameter and never interpolated, which is what lets
    // this table be shared where `PoolTask` had to name a table instead.
    //
    // Keyed by `array_id` and not by name: the name is unique today, but
    // `array_id` is what every other Elastic table's foreign key already
    // points at, and it is the identity that survives.
    //
    // No rebuild here, unlike migration 12: these are new tables, so their
    // CHECK constraints are written once and nothing has to be copied.
    "CREATE TABLE nas_elastic_schedules (
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        kind TEXT NOT NULL CHECK(kind IN ('mover','sync','scrub')),
        enabled INTEGER NOT NULL DEFAULT 0,
        schedule_json TEXT NOT NULL,
        last_run_at TEXT,
        last_result TEXT NOT NULL DEFAULT '',
        next_run_at TEXT,
        PRIMARY KEY (array_id, kind)
    ) WITHOUT ROWID;
    -- The mover's SETTINGS are not a schedule: no cadence, no last run, no
    -- next run. They get their own table so that the EXISTENCE of a row is
    -- the honest answer to `MoverConfig::configured` — with the values on the
    -- array row instead, a column holding 7200 could not be told apart from a
    -- column nobody ever wrote, and the panel would present a built-in default
    -- as somebody's decision.
    CREATE TABLE nas_elastic_mover_settings (
        array_id TEXT PRIMARY KEY REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        min_age_secs INTEGER NOT NULL,
        cache_min_free_pct INTEGER NOT NULL,
        coupled_sync INTEGER NOT NULL
    ) WITHOUT ROWID;",
), (
    14,
    // An adopted array becomes an operation kind of its own. Rebuilt for the
    // same reason as migration 12: `kind` is a CHECK constraint and SQLite
    // cannot widen one in place.
    //
    // WHY a kind and not a second 'create' row: the create operation's
    // `request_json` is where `elastic_spec` reads an array's persisted
    // intention from, so an adopted array MUST have such a row or every read
    // of it fails. Writing that row as 'create' would record this node as
    // having created storage it only took over, with an adoption timestamp
    // standing in for a creation it never performed. The origin row is
    // therefore either a create or an import, and the partial unique index
    // below allows exactly ONE of the two per array.
    "CREATE TABLE nas_elastic_operations_new (
        operation_id TEXT PRIMARY KEY,
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        job_id TEXT NOT NULL UNIQUE REFERENCES nas_jobs(job_id) ON DELETE RESTRICT,
        kind TEXT NOT NULL CHECK(kind IN ('create','import','restore','sync','scrub','mover')),
        state TEXT NOT NULL CHECK(state IN ('running','succeeded','failed','needs_attention')),
        request_json TEXT NOT NULL,
        result_json TEXT,
        error TEXT NOT NULL,
        created_at TEXT NOT NULL,
        finished_at TEXT
    );
    INSERT INTO nas_elastic_operations_new
        SELECT operation_id,array_id,job_id,kind,state,request_json,result_json,error,created_at,finished_at
        FROM nas_elastic_operations;
    DROP TABLE nas_elastic_operations;
    ALTER TABLE nas_elastic_operations_new RENAME TO nas_elastic_operations;
    CREATE INDEX nas_elastic_operation_array ON nas_elastic_operations(array_id, created_at);
    CREATE UNIQUE INDEX nas_elastic_operation_running
        ON nas_elastic_operations(array_id) WHERE state = 'running';
    CREATE UNIQUE INDEX nas_elastic_operation_origin
        ON nas_elastic_operations(array_id) WHERE kind IN ('create','import');",
), (
    15,
    // The three lifecycle operations §5.3 promised and the schema had no room
    // for: repairing one data disk from parity, adding a data disk to a live
    // array, and dissolving the array. A widened CHECK needs the whole table
    // rebuilt (the same shape migration 14 used), so this repeats 14's
    // statement with three kinds added and nothing else changed.
    //
    // NO index of its own for 'add_disk'. The rule that matters — a second add
    // of a DIFFERENT disk while one is unfinished — is `reserve_added_disk`'s,
    // and it cannot be an index: a unique index over the unfinished adds would
    // also refuse the RETRY, which has to be allowed, because the failed
    // attempt's request row is the only record of the filesystem UUID its mkfs
    // stamped on the disk. `nas_elastic_operation_running` already refuses two
    // at once.
    "CREATE TABLE nas_elastic_operations_new (
        operation_id TEXT PRIMARY KEY,
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        job_id TEXT NOT NULL UNIQUE REFERENCES nas_jobs(job_id) ON DELETE RESTRICT,
        kind TEXT NOT NULL CHECK(kind IN
            ('create','import','restore','sync','scrub','mover','fix','add_disk','dissolve')),
        state TEXT NOT NULL CHECK(state IN ('running','succeeded','failed','needs_attention')),
        request_json TEXT NOT NULL,
        result_json TEXT,
        error TEXT NOT NULL,
        created_at TEXT NOT NULL,
        finished_at TEXT
    );
    INSERT INTO nas_elastic_operations_new
        SELECT operation_id,array_id,job_id,kind,state,request_json,result_json,error,created_at,finished_at
        FROM nas_elastic_operations;
    DROP TABLE nas_elastic_operations;
    ALTER TABLE nas_elastic_operations_new RENAME TO nas_elastic_operations;
    CREATE INDEX nas_elastic_operation_array ON nas_elastic_operations(array_id, created_at);
    CREATE UNIQUE INDEX nas_elastic_operation_running
        ON nas_elastic_operations(array_id) WHERE state = 'running';
    CREATE UNIQUE INDEX nas_elastic_operation_origin
        ON nas_elastic_operations(array_id) WHERE kind IN ('create','import');",
), (
    16,
    // Where a folder's cache policy lives. Until now `ElasticArrayRow.folders`
    // was built from nothing on every read, so `mover_rules` derived its
    // `pinned_folders`/`eager_folders` from an empty Vec and the helper — which
    // honours both — was handed two empty lists for every array that has ever
    // existed. §5.3's per-folder switch was typed, transported and executed,
    // and there was no way to create one.
    //
    // A new table, so the CHECK constraints are written once and nothing is
    // rebuilt (migration 13's shape, not 12's).
    //
    // ONLY THE EXCEPTIONS ARE STORED. 'yes' is the default and is spelled by
    // the ABSENCE of a row, which is why the CHECK admits 'no' and 'only' and
    // not 'yes': with all three storable, a folder could be 'yes' in two ways
    // at once and the two spellings could then disagree about which one the
    // mover read. Returning a folder to the default is a DELETE.
    //
    // The folder is ONE top-level name and the constraints say so in SQL, not
    // only in Rust: it becomes a mover rule the helper resolves under the
    // union, so a row holding 'a/b' or '..' would be a path this node handed
    // to a privileged step. `nas_elastic_folders` would have been the obvious
    // name and is deliberately not used — this table holds a policy per
    // folder, never the folders themselves, which are discovered by reading
    // the union (`elastic::folders_of`).
    "CREATE TABLE nas_elastic_folder_cache (
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        folder TEXT NOT NULL CHECK(
            length(folder) > 0 AND length(folder) <= 255
            AND folder NOT LIKE '%/%' AND folder NOT IN ('.','..')
            AND folder = trim(folder, char(9,10,13,0))
        ),
        cache_policy TEXT NOT NULL CHECK(cache_policy IN ('no','only')),
        PRIMARY KEY (array_id, folder)
    ) WITHOUT ROWID;",
), (
    17,
    // Replacing the disk of one data slot and rebuilding the array onto it —
    // the operation an array whose disk died needs and the schema had no kind
    // for. A widened CHECK needs the whole table rebuilt, so this repeats
    // migration 15's statement with one kind added and nothing else changed.
    //
    // NO index of its own, for the reason 15 gives for 'add_disk': the rule
    // that matters is "one running operation per array", which
    // `nas_elastic_operation_running` already enforces, and a unique index
    // over unfinished replacements would refuse the REPEAT that finishes a
    // replacement interrupted between the journal swap and the rebuild.
    "CREATE TABLE nas_elastic_operations_new (
        operation_id TEXT PRIMARY KEY,
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        job_id TEXT NOT NULL UNIQUE REFERENCES nas_jobs(job_id) ON DELETE RESTRICT,
        kind TEXT NOT NULL CHECK(kind IN
            ('create','import','restore','sync','scrub','mover','fix','add_disk','replace_disk','dissolve')),
        state TEXT NOT NULL CHECK(state IN ('running','succeeded','failed','needs_attention')),
        request_json TEXT NOT NULL,
        result_json TEXT,
        error TEXT NOT NULL,
        created_at TEXT NOT NULL,
        finished_at TEXT
    );
    INSERT INTO nas_elastic_operations_new
        SELECT operation_id,array_id,job_id,kind,state,request_json,result_json,error,created_at,finished_at
        FROM nas_elastic_operations;
    DROP TABLE nas_elastic_operations;
    ALTER TABLE nas_elastic_operations_new RENAME TO nas_elastic_operations;
    CREATE INDEX nas_elastic_operation_array ON nas_elastic_operations(array_id, created_at);
    CREATE UNIQUE INDEX nas_elastic_operation_running
        ON nas_elastic_operations(array_id) WHERE state = 'running';
    CREATE UNIQUE INDEX nas_elastic_operation_origin
        ON nas_elastic_operations(array_id) WHERE kind IN ('create','import');",
), (
    18,
    // WHICH ORGANISATION a job or an alert belongs to. This database is ONE
    // per node, not one per tenant: TentaNas is a singleton package, every
    // organisation's request resolves to the same instance id, and
    // `app_db::open` keys its pool by that id alone — so every tenant of the
    // node reads these two tables. Elastic Arrays are the one thing a tenant
    // owns (`nas_elastic_arrays.org_id`), and a job or an alert about another
    // tenant's array names that array, its user and its log.
    //
    // NULL is a node-wide row (a disk, a pool, a target: shared hardware every
    // admin of the node may see). A non-NULL value is the owning organisation,
    // and '' is a row that must be owned but whose owner could not be found
    // (an array that no longer exists): shown to NO tenant, because guessing
    // an owner is how another tenant's array would leak.
    //
    // Backfill: an Elastic job by its operation's array, then by the array
    // holding its subject name; a disk wipe only when its log names a
    // released journal, the one wipe line that names an array; an alert by
    // the array it is about or the parked request it announces.
    //
    // A NAME IS UNIQUE ONLY AMONG THE ARRAYS THAT EXIST NOW. A dissolve
    // deletes the array row and its operations (`delete_elastic_array`) and
    // keeps the jobs and the alerts, so org A's dissolved `media` and org B's
    // later `media` share one name in the history. The name match therefore
    // takes only an array that already existed when the row was written
    // (`created_at <= started_at` / `raised_at`); an older row finds no array
    // and becomes '' — nobody's — instead of org B's. The comparison is
    // textual and valid because all three columns are written by `now()`,
    // the one fixed-width `YYYY-MM-DDTHH:MM:SSZ` UTC form (a create writes the
    // array's `created_at` from its job's `started_at`, so they are equal).
    "ALTER TABLE nas_jobs ADD COLUMN org_id TEXT;
    ALTER TABLE nas_alerts ADD COLUMN org_id TEXT;
    UPDATE nas_jobs SET org_id = COALESCE(
            (SELECT a.org_id FROM nas_elastic_operations o
               JOIN nas_elastic_arrays a ON a.array_id = o.array_id
              WHERE o.job_id = nas_jobs.job_id),
            (SELECT a.org_id FROM nas_elastic_arrays a
              WHERE a.name = nas_jobs.subject AND a.created_at <= nas_jobs.started_at),
            '')
     WHERE substr(kind, 1, 8) = 'elastic_';
    UPDATE nas_jobs SET org_id = ''
     WHERE kind = 'disk_wipe' AND instr(log, 'zwolniono rezerwację dziennika') > 0;
    UPDATE nas_alerts SET org_id = COALESCE(
            (SELECT a.org_id FROM nas_elastic_arrays a
              WHERE a.name = nas_alerts.subject_id AND a.created_at <= nas_alerts.raised_at), '')
     WHERE subject_kind = 'elastic-array';
    UPDATE nas_alerts SET org_id = COALESCE(
            (SELECT p.org_id FROM nas_pending_approvals p WHERE p.request_id = nas_alerts.subject_id), '')
     WHERE subject_kind = 'approval';",
), (
    19,
    // SCRUB IS ON BY DEFAULT (owner decision 2026-09-22), and this is the
    // one-time half of it: every array that EXISTS when a node upgrades gets
    // the default monthly scrub, the same row a new array now gets at creation
    // (`insert_default_scrub_schedule`). Parity SYNC already happens without
    // anyone (the mover's coupled sync); what never happened unless an admin
    // armed it is the scrub — the read of every block against parity that is
    // the only thing finding silent corruption before a disk has to be rebuilt
    // from it.
    //
    // ONCE, BECAUSE IT IS A MIGRATION. The runner records version 19 and never
    // runs it again, so a schedule an admin later disables or changes is never
    // touched by it; `INSERT OR IGNORE` on the `(array_id, kind)` key means an
    // array that ALREADY has a scrub row — armed, disabled or re-timed by
    // somebody — keeps it exactly as it is, even here. No marker row of its
    // own: `app_schema_version` already is that marker, and a second one
    // could only disagree with it.
    //
    // Only arrays WITH PARITY: SnapRAID has nothing to compare against on an
    // array without a parity disk, and `insert_job` refuses its scrub
    // ("Macierz bez parity nie wykonuje SnapRAID") — a default there would be
    // a row that fails every month.
    //
    // `next_run_at` stays NULL: the scheduler computes it on its next tick
    // and never fires retroactively (`scheduler::is_due`), so an upgrade does
    // not start a scrub on every array at once.
    concat!(
        "INSERT OR IGNORE INTO nas_elastic_schedules (array_id, kind, enabled, schedule_json)
        SELECT a.array_id, 'scrub', 1, '",
        default_elastic_scrub_schedule_json!(),
        "'
          FROM nas_elastic_arrays a
         WHERE EXISTS (SELECT 1 FROM nas_elastic_disks d
                        WHERE d.array_id = a.array_id AND d.role = 'parity');"
    ),
), (
    20,
    // WHICH ORGANISATION a share, a share account and a block target belong
    // to (owner decision 2026-09-22): the organisation that created it, and
    // only that one sees and manages it. Pools and disks stay node hardware.
    // `nas_access_events` carries the owner too, stamped from the share when
    // the line is COLLECTED: a deleted share's log keeps its owner, and a
    // later share of another organisation under the same name never inherits
    // it. Share grants and target allowlists are children reached only
    // through their parent's id, so they need no column of their own.
    //
    // NOT NULL DEFAULT '': every write path stamps the owner; a row that
    // somehow arrives without one is '' — owned but unknown — and is shown to
    // NO organisation (the `''` rule of migration 18). Failing closed is the
    // point: a forgotten stamp hides a row, it never hands it to a tenant.
    //
    // THE BACKFILL. Every existing row was created before ownership existed,
    // by an admin of SOME organisation of this node, and all of them were
    // visible to every tenant until now. Two rules, in this order:
    //
    //   1. A share whose source is an Elastic Array union (or a folder in it)
    //      belongs to that array's organisation. The array IS owned, and until
    //      this migration only that organisation could create a share on it
    //      (wave 2 made `share_create` resolve against the caller's arrays),
    //      so this is the one owner the rows can prove. An array whose own
    //      owner is EMPTY proves nothing, so its shares fall to rule 2: ''
    //      would hide a live export from every tenant (see below). No code
    //      path is known to write such an array; the fallback is for the one
    //      nobody knows about.
    //   2. Everything else — shares on pool datasets, every block target —
    //      goes to the node's DEFAULT organisation ('org-default',
    //      `services::org::DEFAULT_ORG_ID`, pinned by a test). It is the org
    //      the platform's own migration v32 backfilled every historical row
    //      to, the org this singleton's database lives under
    //      (`app_db::data_dir_org`), and on a node with a single organisation
    //      it IS that organisation — rig11 on 2026-09-23 has exactly one.
    //
    // WHY NOT '' (hidden) for rule 2, as migration 18 did for jobs of a gone
    // array: a job is history, a share is a LIVE export. A share nobody can
    // see is still served by smbd, still mounted fleet-wide, and cannot be
    // paused, edited or deleted from any tenant's UI — the least manageable
    // state there is. And rule 2 reveals nothing new: every tenant could read
    // and change these rows before this migration; after it, the other
    // tenants lose that access and the default organisation keeps it.
    //
    // Share accounts follow their grants: an account granted ONLY on shares
    // of one organisation is that organisation's; one with no grants, or with
    // grants in more than one organisation, is the default organisation's.
    // Access events follow their share by name; a line of a share that no
    // longer exists goes to the default organisation, the rule-2 owner of a
    // share nobody can attribute. Share, target and import JOBS follow the
    // same rows by subject name, and only a row that already existed when the
    // job started is taken (the name-reuse guard of migration 18); a target's
    // alerts follow the target the same way.
    "ALTER TABLE nas_shares ADD COLUMN org_id TEXT NOT NULL DEFAULT '';
    ALTER TABLE nas_share_users ADD COLUMN org_id TEXT NOT NULL DEFAULT '';
    ALTER TABLE nas_targets ADD COLUMN org_id TEXT NOT NULL DEFAULT '';
    ALTER TABLE nas_access_events ADD COLUMN org_id TEXT NOT NULL DEFAULT '';
    UPDATE nas_shares SET org_id = COALESCE(
            (SELECT NULLIF(a.org_id, '') FROM nas_elastic_arrays a
              WHERE nas_shares.source_path = '/mnt/' || a.name
                 OR substr(nas_shares.source_path, 1, length(a.name) + 6) = '/mnt/' || a.name || '/'),
            'org-default');
    UPDATE nas_share_users SET org_id = COALESCE(
            (SELECT MIN(s.org_id) FROM nas_share_grants g
               JOIN nas_shares s ON s.share_id = g.share_id
              WHERE g.user = nas_share_users.name
             HAVING COUNT(DISTINCT s.org_id) = 1),
            'org-default');
    UPDATE nas_targets SET org_id = 'org-default';
    UPDATE nas_access_events SET org_id = COALESCE(
            (SELECT s.org_id FROM nas_shares s WHERE s.name = nas_access_events.share),
            'org-default');
    UPDATE nas_jobs SET org_id = COALESCE(
            (SELECT s.org_id FROM nas_shares s
              WHERE s.name = nas_jobs.subject AND s.created_at <= nas_jobs.started_at),
            'org-default')
     WHERE kind IN ('share_create', 'share_update', 'share_delete') AND org_id IS NULL;
    UPDATE nas_jobs SET org_id = COALESCE(
            (SELECT t.org_id FROM nas_targets t
              WHERE t.name = nas_jobs.subject AND t.created_at <= nas_jobs.started_at),
            'org-default')
     WHERE kind IN ('target_create', 'target_update', 'target_delete') AND org_id IS NULL;
    UPDATE nas_jobs SET org_id = 'org-default' WHERE kind = 'config_import' AND org_id IS NULL;
    UPDATE nas_alerts SET org_id = COALESCE(
            (SELECT t.org_id FROM nas_targets t
              WHERE t.name = nas_alerts.subject_id AND t.created_at <= nas_alerts.raised_at),
            'org-default')
     WHERE subject_kind = 'target';
    CREATE INDEX nas_shares_org ON nas_shares(org_id);
    CREATE INDEX nas_share_users_org ON nas_share_users(org_id);
    CREATE INDEX nas_targets_org ON nas_targets(org_id);
    CREATE INDEX nas_access_events_org ON nas_access_events(org_id, at DESC);",
), (
    21,
    // Codes instead of English (M1). The screens word a disk's health and an
    // alert in the reader's language from a code and its parameters; the
    // English columns stay as the tooltip, the forwarded syslog/webhook text,
    // and what an older build reads.
    //
    // `nas_disks.health_reasons` is SMART's verdict as `NasHealthReason[]`
    // JSON, written beside `health_reason` by the same statement. Before it a
    // restart could only re-derive the codes from the stored document, and
    // lost them whenever the week-old sample had moved on. Old rows get '[]':
    // the screen shows the translated grade with the stored sentence as its
    // tooltip until the next SMART read writes both.
    //
    // `nas_alerts.code` / `params` (JSON object of strings) / `reasons`
    // (`NasHealthReason[]` JSON): see `NasAlert`. Old rows are backfilled
    // only where the code follows from structured columns alone, never from
    // the English text:
    //   * a parked approval's alert — its request row names the operation
    //     and the subject;
    //   * a disk's health alert — its severity IS the grade. An OPEN row is
    //     named by the disk row's name, marked 'last_known': every raise of
    //     an open row rewrote it with the name the disk had then, the disk row
    //     keeps the name it was last seen under, and nothing here proves the
    //     disk is still present (the first inventory pass after the upgrade
    //     rewrites a present disk's alert with its live name and reasons). A
    //     RESOLVED row gets no name ('unknown'): sdX names move between
    //     reboots, so today's name of its disk is not the name it was raised
    //     under, and no structured column kept that one (its English title is
    //     not parsed). No reasons are backfilled: the detail sentence is not
    //     parsed back into codes either.
    //   * the node-wide target sweep alert ('targets:reconcile'), OPEN AND
    //     RESOLVED: an older build wrote "{name}: {error}" for every failing
    //     target of every organisation into this row, which every tenant
    //     lists (`alerts_for_subject` returns resolved rows too). Its text is
    //     replaced by the neutral one `targets::rewrite_stale_sweep_alert`
    //     gives an open row at startup — the same title and detail, byte for
    //     byte (a test pins them to targets.rs) — and coded
    //     'targets_sweep_stale': no count that anyone measured.
    // Every other row keeps code '' and is shown as an uncoded alert.
    "ALTER TABLE nas_disks ADD COLUMN health_reasons TEXT NOT NULL DEFAULT '[]';
    ALTER TABLE nas_alerts ADD COLUMN code TEXT NOT NULL DEFAULT '';
    ALTER TABLE nas_alerts ADD COLUMN params TEXT NOT NULL DEFAULT '{}';
    ALTER TABLE nas_alerts ADD COLUMN reasons TEXT NOT NULL DEFAULT '[]';
    UPDATE nas_alerts SET code = 'approval_pending',
           params = (SELECT json_object('operation', p.operation, 'subject', p.subject)
                       FROM nas_pending_approvals p WHERE p.request_id = nas_alerts.subject_id)
     WHERE subject_kind = 'approval'
       AND EXISTS (SELECT 1 FROM nas_pending_approvals p WHERE p.request_id = nas_alerts.subject_id);
    UPDATE nas_alerts SET code = 'disk_health',
           params = COALESCE(
               (SELECT json_object('health', nas_alerts.severity, 'name', d.name, 'name_source', 'last_known')
                  FROM nas_disks d WHERE d.disk_id = nas_alerts.subject_id AND trim(d.name) <> ''
                   AND nas_alerts.resolved_at IS NULL),
               json_object('health', nas_alerts.severity, 'name_source', 'unknown'))
     WHERE subject_kind = 'disk'
       AND dedupe_key = 'disk:' || subject_id || ':health'
       AND severity IN ('warning', 'critical');
    UPDATE nas_alerts SET code = 'targets_sweep_stale', params = '{}', reasons = '[]',
           title = 'Block targets: this node cannot reach the state it decided on',
           detail = 'block targets on this node were not in the state it decided on before it restarted; \
the next reconcile replaces this with the current count'
     WHERE dedupe_key = 'targets:reconcile';",
), (
    22,
    // The UNDO of an unfinished add of a data disk ('add_disk_abort', helper
    // 0.14.0): the operation that removes the slot again while the disk has
    // provably never joined the share. A widened CHECK needs the whole table
    // rebuilt, so this repeats migration 17's statement with one kind added
    // and nothing else changed.
    "CREATE TABLE nas_elastic_operations_new (
        operation_id TEXT PRIMARY KEY,
        array_id TEXT NOT NULL REFERENCES nas_elastic_arrays(array_id) ON DELETE RESTRICT,
        job_id TEXT NOT NULL UNIQUE REFERENCES nas_jobs(job_id) ON DELETE RESTRICT,
        kind TEXT NOT NULL CHECK(kind IN
            ('create','import','restore','sync','scrub','mover','fix','add_disk','add_disk_abort',
             'replace_disk','dissolve')),
        state TEXT NOT NULL CHECK(state IN ('running','succeeded','failed','needs_attention')),
        request_json TEXT NOT NULL,
        result_json TEXT,
        error TEXT NOT NULL,
        created_at TEXT NOT NULL,
        finished_at TEXT
    );
    INSERT INTO nas_elastic_operations_new
        SELECT operation_id,array_id,job_id,kind,state,request_json,result_json,error,created_at,finished_at
        FROM nas_elastic_operations;
    DROP TABLE nas_elastic_operations;
    ALTER TABLE nas_elastic_operations_new RENAME TO nas_elastic_operations;
    CREATE INDEX nas_elastic_operation_array ON nas_elastic_operations(array_id, created_at);
    CREATE UNIQUE INDEX nas_elastic_operation_running
        ON nas_elastic_operations(array_id) WHERE state = 'running';
    CREATE UNIQUE INDEX nas_elastic_operation_origin
        ON nas_elastic_operations(array_id) WHERE kind IN ('create','import');",
)];

/// The owner migration 20 gives a legacy share, target or account it cannot
/// attribute. Spelled here once for the test that pins the migration's SQL
/// literal to the platform constant.
#[cfg(test)]
const LEGACY_OWNER: &str = "org-default";

/// How far back a disk's health history reaches, and how much of it keeps
/// minute resolution. Both are answers the frontend labels its charts with,
/// so they live here rather than in three call sites.
pub const HISTORY_DAYS: u32 = 30;
const MINUTE_RETENTION_HOURS: i64 = 48;

/// The SMART self-test schedule is one JSON document, not a table: it has
/// exactly one row per node and no keys to index by.
pub const SETTING_SMART_SCHEDULE: &str = "smart_schedule";

/// `app_db::Migrate` for the TentaNas instance database.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "synchronous", "FULL")?;
    anyhow::ensure!(conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))? == 2,
        "NAS wymaga synchronous=FULL");
    crate::addon::app_db::run_versioned_migrations(conn, APP, MIGRATIONS)
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn write(pool: &DbPool) -> Result<parking_lot::MutexGuard<'_, Connection>> {
    pool.write().map_err(|e| anyhow!("tentanas db lock: {e}"))
}

// ----- settings ---------------------------------------------------------------

pub fn setting(pool: &DbPool, key: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT value FROM nas_settings WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn set_setting(pool: &DbPool, key: &str, value: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, now()],
    )?;
    Ok(())
}

/// Adds one to a numeric setting and returns the new value, in a single
/// statement: the privilege-channel counter is bumped from every request
/// handler and a read-modify-write would lose invocations under load.
pub fn bump_counter(pool: &DbPool, key: &str) -> Result<u64> {
    let conn = write(pool)?;
    let value: i64 = conn.query_row(
        "INSERT INTO nas_settings (key, value, updated_at) VALUES (?1, '1', ?2)
         ON CONFLICT(key) DO UPDATE SET
            value = CAST(CAST(value AS INTEGER) + 1 AS TEXT),
            updated_at = excluded.updated_at
         RETURNING CAST(value AS INTEGER)",
        params![key, now()],
        |r| r.get(0),
    )?;
    Ok(value.max(0) as u64)
}

// ----- environment cache -------------------------------------------------------

pub fn cached_environment(pool: &DbPool) -> Result<Option<(String, String)>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT json, probed_at FROM nas_environment WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

pub fn store_environment(pool: &DbPool, json: &str, probed_at: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_environment (id, json, probed_at) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json, probed_at = excluded.probed_at",
        params![json, probed_at],
    )?;
    Ok(())
}

// ----- disks -------------------------------------------------------------------

/// Identity of a disk as the inventory last saw it. `smart_json` is the raw
/// `smartctl --json=c -x` document, the source of attributes and self-tests
/// on the detail view — it is parsed on demand rather than normalized into
/// columns because the attribute table differs per vendor.
pub struct DiskRow {
    pub disk_id: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub smart_json: Option<String>,
    pub smart_read_at: Option<String>,
    pub health: String,
    pub health_reason: String,
    /// SMART's verdict as codes, the ones `health_reason` is the sentence
    /// of (migration 21). Empty for a row written before the column existed,
    /// and for one whose JSON does not parse — no codes is an honest gap (the
    /// screen shows the grade), a guessed code is not.
    pub health_reasons: Vec<NasHealthReason>,
}

pub struct DiskIdentity<'a> {
    pub disk_id: &'a str,
    pub name: &'a str,
    pub model: &'a str,
    pub serial: &'a str,
    pub wwn: Option<&'a str>,
    pub size_bytes: u64,
    pub kind: &'a str,
}

pub fn upsert_disk_seen(pool: &DbPool, disk: &DiskIdentity<'_>) -> Result<()> {
    let conn = write(pool)?;
    let ts = now();
    conn.execute(
        "INSERT INTO nas_disks (disk_id, name, model, serial, wwn, size_bytes, kind,
                                first_seen_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(disk_id) DO UPDATE SET
            name = excluded.name, model = excluded.model, serial = excluded.serial,
            wwn = excluded.wwn, size_bytes = excluded.size_bytes, kind = excluded.kind,
            last_seen_at = excluded.last_seen_at",
        params![
            disk.disk_id,
            disk.name,
            disk.model,
            disk.serial,
            disk.wwn,
            disk.size_bytes as i64,
            disk.kind,
            ts
        ],
    )?;
    Ok(())
}

pub fn disk_row(pool: &DbPool, disk_id: &str) -> Result<Option<DiskRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT disk_id, first_seen_at, last_seen_at, smart_json, smart_read_at,
                    health, health_reason, health_reasons
             FROM nas_disks WHERE disk_id = ?1",
            params![disk_id],
            |r| {
                let reasons: String = r.get(7)?;
                Ok(DiskRow {
                    disk_id: r.get(0)?,
                    first_seen_at: r.get(1)?,
                    last_seen_at: r.get(2)?,
                    smart_json: r.get(3)?,
                    smart_read_at: r.get(4)?,
                    health: r.get(5)?,
                    health_reason: r.get(6)?,
                    health_reasons: serde_json::from_str(&reasons).unwrap_or_default(),
                })
            },
        )
        .optional()?)
}

/// The kernel name a disk was last seen under, kept after it leaves the live
/// inventory. `None` for a disk never recorded, or recorded without a name.
pub fn disk_last_name(pool: &DbPool, disk_id: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT name FROM nas_disks WHERE disk_id = ?1",
            params![disk_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .filter(|name| !name.is_empty()))
}

/// Stores one SMART read: the document, SMART's grade, and its reasons both
/// as the English sentence and as codes. One statement, so a restart never
/// reads a sentence and codes from two different reads.
pub fn store_smart(
    pool: &DbPool,
    disk_id: &str,
    smart_json: &str,
    health: &str,
    health_reasons: &[NasHealthReason],
) -> Result<()> {
    let health_reason = super::disks::disk_reasons_text(health_reasons);
    let reasons_json = serde_json::to_string(health_reasons)?;
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_disks SET smart_json = ?2, smart_read_at = ?3, health = ?4, health_reason = ?5,
                              health_reasons = ?6
         WHERE disk_id = ?1",
        params![disk_id, smart_json, now(), health, health_reason, reasons_json],
    )?;
    Ok(())
}

// ----- samples -----------------------------------------------------------------

pub struct SampleInsert<'a> {
    pub disk_id: &'a str,
    pub at: &'a str,
    pub temperature_c: Option<i32>,
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
    pub crc_errors: Option<u64>,
    pub media_errors: Option<u64>,
    pub read_bps: u64,
    pub write_bps: u64,
    pub await_ms: f64,
}

pub fn insert_samples(pool: &DbPool, samples: &[SampleInsert<'_>]) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO nas_disk_samples
                (disk_id, at, temperature_c, reallocated, pending, crc_errors, media_errors,
                 read_bps, write_bps, await_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for s in samples {
            stmt.execute(params![
                s.disk_id,
                s.at,
                s.temperature_c,
                s.reallocated.map(|v| v as i64),
                s.pending.map(|v| v as i64),
                s.crc_errors.map(|v| v as i64),
                s.media_errors.map(|v| v as i64),
                s.read_bps as i64,
                s.write_bps as i64,
                s.await_ms,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn sample_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NasDiskSample> {
    Ok(NasDiskSample {
        at: r.get(0)?,
        temperature_c: r.get(1)?,
        reallocated_sectors: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
        pending_sectors: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        read_bps: r.get::<_, i64>(4)? as u64,
        write_bps: r.get::<_, i64>(5)? as u64,
        await_ms: r.get(6)?,
    })
}

/// Minute samples of one disk since `since` (RFC 3339), oldest first.
pub fn samples_since(pool: &DbPool, disk_id: &str, since: &str) -> Result<Vec<NasDiskSample>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT at, temperature_c, reallocated, pending, read_bps, write_bps, await_ms
         FROM nas_disk_samples WHERE disk_id = ?1 AND at >= ?2 ORDER BY at",
    )?;
    let rows = stmt
        .query_map(params![disk_id, since], sample_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The whole history of one disk since `since`: hourly rows for everything
/// older than the minute window, minute rows after it. The two tables never
/// overlap — downsampling writes an hour's row in the same call that deletes
/// its minutes — so a plain concatenation is already ordered and gap-free.
pub fn history_since(pool: &DbPool, disk_id: &str, since: &str) -> Result<Vec<NasDiskSample>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT at, temperature_c, reallocated, pending, read_bps, write_bps, await_ms
         FROM nas_disk_hourly WHERE disk_id = ?1 AND at >= ?2
         UNION ALL
         SELECT at, temperature_c, reallocated, pending, read_bps, write_bps, await_ms
         FROM nas_disk_samples WHERE disk_id = ?1 AND at >= ?2
         ORDER BY at",
    )?;
    let rows = stmt
        .query_map(params![disk_id, since], sample_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The raw value of one SMART attribute as it was ~7 days ago: the "trend"
/// column of the attribute table (§5.4).
pub fn attribute_week_ago(pool: &DbPool, disk_id: &str, column: &str) -> Result<Option<i64>> {
    // Column name comes from a fixed set in disks.rs, never from the wire.
    debug_assert!(["reallocated", "pending", "crc_errors", "media_errors"].contains(&column));
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let since = (chrono::Utc::now() - chrono::Duration::days(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // A week ago is outside the minute window, so the hourly table is where
    // the answer normally is; both are searched because the minute table still
    // holds everything on a node younger than the retention cutoff.
    Ok(conn
        .query_row(
            &format!(
                "SELECT {column} FROM (
                     SELECT at, {column} FROM nas_disk_hourly
                      WHERE disk_id = ?1 AND at <= ?2 AND {column} IS NOT NULL
                     UNION ALL
                     SELECT at, {column} FROM nas_disk_samples
                      WHERE disk_id = ?1 AND at <= ?2 AND {column} IS NOT NULL
                 ) ORDER BY at DESC LIMIT 1"
            ),
            params![disk_id, since],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// Retention (§5.4): 48 h of minute samples, then hourly rows out to 30 days.
/// Downsampling and pruning are ONE call and one transaction on purpose — an
/// hour whose minutes were deleted before its hourly row was written would be
/// a permanent hole in the chart.
pub fn prune_samples(pool: &DbPool) -> Result<usize> {
    let mut conn = write(pool)?;
    let now = chrono::Utc::now();
    // Aligned to the hour, and the SAME boundary decides both statements: an
    // hour is downsampled only once it is entirely behind the window, so a
    // later run can never replace a full hour's row with the average of the
    // few minutes that had not crossed the boundary yet.
    let minute_cutoff = (now - chrono::Duration::hours(MINUTE_RETENTION_HOURS))
        .format("%Y-%m-%dT%H:00:00Z")
        .to_string();
    let history_cutoff = (now - chrono::Duration::days(i64::from(HISTORY_DAYS)))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let tx = conn.transaction()?;
    // The counters are monotonic, so an hour's value is its MAX; throughput,
    // temperature and latency are averages of the minutes in it.
    tx.execute(
        "INSERT OR REPLACE INTO nas_disk_hourly
            (disk_id, at, temperature_c, reallocated, pending, crc_errors, media_errors,
             read_bps, write_bps, await_ms)
         SELECT disk_id,
                substr(at, 1, 13) || ':00:00Z',
                CAST(ROUND(AVG(temperature_c)) AS INTEGER),
                MAX(reallocated), MAX(pending), MAX(crc_errors), MAX(media_errors),
                CAST(ROUND(AVG(read_bps)) AS INTEGER),
                CAST(ROUND(AVG(write_bps)) AS INTEGER),
                AVG(await_ms)
           FROM nas_disk_samples
          WHERE at < ?1 AND at >= ?2
          GROUP BY disk_id, substr(at, 1, 13)",
        params![minute_cutoff, history_cutoff],
    )?;
    let dropped = tx.execute(
        "DELETE FROM nas_disk_samples WHERE at < ?1",
        params![minute_cutoff],
    )?;
    let expired = tx.execute(
        "DELETE FROM nas_disk_hourly WHERE at < ?1",
        params![history_cutoff],
    )?;
    tx.commit()?;
    Ok(dropped + expired)
}

// ----- alerts ------------------------------------------------------------------

/// A column of JSON the node wrote itself (`params`, `reasons`), read back
/// leniently: a row that does not parse loses its codes and is shown as an
/// uncoded alert — its English title and detail are still there — rather
/// than failing the whole alert list over one row.
fn alert_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NasAlert> {
    let code: String = r.get(9)?;
    let params: String = r.get(10)?;
    let reasons: String = r.get(11)?;
    // A code without its parameters would be worded with holes, so an
    // unparsable row is uncoded as a whole.
    let parsed = (
        serde_json::from_str::<BTreeMap<String, String>>(&params),
        serde_json::from_str::<Vec<NasHealthReason>>(&reasons),
    );
    let (code, params, reasons) = match parsed {
        (Ok(params), Ok(reasons)) => (code, params, reasons),
        _ => (String::new(), BTreeMap::new(), Vec::new()),
    };
    Ok(NasAlert {
        alert_id: r.get(0)?,
        severity: r.get(1)?,
        subject_kind: r.get(2)?,
        subject_id: r.get(3)?,
        title: r.get(4)?,
        detail: r.get(5)?,
        raised_at: r.get(6)?,
        acked_at: r.get(7)?,
        resolved_at: r.get(8)?,
        code,
        params,
        reasons,
    })
}

const ALERT_COLUMNS: &str = "alert_id, severity, subject_kind, subject_id, title, detail, \
                             raised_at, acked_at, resolved_at, code, params, reasons";

/// What an alert says, in both forms: a code with parameters (and coded
/// detail lines) that the screens word in the reader's language, and the
/// node's own English title and detail — the text syslog and webhook
/// forwarding send (machine-facing), what an older build shows, and the
/// tooltip. Every raiser fills both, so neither reader is left without.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlertText {
    pub code: String,
    pub params: BTreeMap<String, String>,
    pub reasons: Vec<NasHealthReason>,
    pub title: String,
    pub detail: String,
}

impl AlertText {
    pub fn new(code: &str, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { code: code.to_string(), title: title.into(), detail: detail.into(), ..Self::default() }
    }

    /// One parameter, in its decimal or wire spelling (`NasAlert::params`).
    pub fn param(mut self, key: &str, value: impl ToString) -> Self {
        self.params.insert(key.to_string(), value.to_string());
        self
    }

    pub fn reasons(mut self, reasons: Vec<NasHealthReason>) -> Self {
        self.reasons = reasons;
        self
    }
}

/// The organisation an alert belongs to, as an SQL expression over two
/// `raise_alert` parameters: `kind` names the subject kind's placeholder and
/// `subject` the subject id's. See migration 18: NULL for shared hardware, the
/// owner's org for an alert about an Elastic Array (by the name an EXISTING
/// array holds right now) or a parked red-path request, and '' — nobody's —
/// when that owner cannot be found, so an alert about an array whose row is
/// gone is never shown to a tenant it might not belong to.
///
/// A function over the placeholders rather than one constant because the
/// insert and the refresh of `raise_alert` bind their parameters in different
/// positions, and both must stamp the owner with the very same rule.
///
/// A block TARGET is owned since migration 20: its portal-drift alert names
/// the target and describes its network, so it goes to the target's
/// organisation (subject = the target's name, unique on the node).
fn alert_owner_sql(kind: &str, subject: &str) -> String {
    format!(
        "CASE {kind} \
         WHEN 'elastic-array' THEN COALESCE((SELECT org_id FROM nas_elastic_arrays WHERE name = {subject}), '') \
         WHEN 'approval' THEN COALESCE((SELECT org_id FROM nas_pending_approvals WHERE request_id = {subject}), '') \
         WHEN 'target' THEN COALESCE((SELECT org_id FROM nas_targets WHERE name = {subject}), '') \
         END"
    )
}

/// The rows one organisation may see: its own and the node-wide ones. Never
/// the '' rows (owner unknown) and never another organisation's.
/// The `?1 <> ''` half keeps an empty org id (a request with no real tenant)
/// from matching the unowned rows.
const VISIBLE_TO_ORG_SQL: &str = "(org_id IS NULL OR (org_id = ?1 AND ?1 <> ''))";

/// Raises an alert unless an open one with the same `dedupe_key` exists, and
/// REFRESHES the text of the one that does. Returns true when a new row was
/// inserted — an already-open alert is not a new event.
///
/// WHY the refresh: the detail is where the whole value of some alerts lives.
/// A portal-drift alert says which interface the address went to; when it
/// moves again from `lan0` to `mgmt0` while the alert is still open, an
/// `INSERT OR IGNORE` would leave the admin reading about `lan0` forever.
/// `raised_at` is deliberately NOT touched for a refresh under the SAME
/// owner — the condition began when it began, and moving the timestamp would
/// hide how long it has been true.
///
/// The refresh RE-STAMPS THE OWNER as well. Array alerts are deduplicated by
/// the array's NAME, a dissolve does not resolve every one of them, and a
/// name can be taken again by another organisation: without the re-stamp,
/// org B's condition would rewrite the text of an alert org A still owns —
/// A reading about B's array, B never seeing it. The owner is also a reason
/// to refresh on its own: identical text raised for a new owner still moves
/// the row to that owner.
///
/// WHEN THE OWNER CHANGES, `acked_at` and `raised_at` are reset too:
/// org A may have acknowledged the old condition, and org B's new one must
/// not arrive pre-acked — B's badge and unacked list would never show it.
/// `raised_at` moves to now for the same reason `acked_at` opens: this is a
/// new condition for the new owner, not a continuation of A's, so its
/// "since" time must be B's, not A's. Neither is touched when the owner is
/// unchanged, which is why the CASE compares the OLD `org_id` (read before
/// this statement's own SET runs) against the freshly computed owner.
///
/// The SUBJECT is refreshed too. The dedupe key is what identifies the
/// condition (a target's alert is keyed by its id), while `subject_id` is the
/// name it is shown and owned by: a target renamed while its alert is open
/// must not keep naming — and being owned through — the old name.
///
/// The CODES are part of the text (migration 21): a refresh rewrites `code`,
/// `params` and `reasons` with the English, and a change in any of them is
/// a reason to refresh — the first pass after an upgrade codes an open row
/// whose English did not change at all. `params` is a `BTreeMap`, so its
/// JSON is written in one key order and equal parameters compare equal.
pub fn raise_coded_alert(
    pool: &DbPool,
    dedupe_key: &str,
    severity: &str,
    subject_kind: &str,
    subject_id: &str,
    text: &AlertText,
) -> Result<bool> {
    let params_json = serde_json::to_string(&text.params)?;
    let reasons_json = serde_json::to_string(&text.reasons)?;
    let conn = write(pool)?;
    let owner = alert_owner_sql("?5", "?6");
    // Bound once and reused for the refresh's `raised_at` below: the insert
    // path still calls `now()` itself, so this is not a shared timestamp,
    // only a name that does not shadow the function.
    let refresh_now = now();
    let updated = conn.execute(
        &format!(
            "UPDATE nas_alerts SET severity = ?2, title = ?3, detail = ?4, org_id = ({owner}),
                 subject_id = ?6, code = ?8, params = ?9, reasons = ?10,
                 acked_at = CASE WHEN org_id IS NOT ({owner}) THEN NULL ELSE acked_at END,
                 raised_at = CASE WHEN org_id IS NOT ({owner}) THEN ?7 ELSE raised_at END
             WHERE dedupe_key = ?1 AND resolved_at IS NULL
               AND (severity <> ?2 OR title <> ?3 OR detail <> ?4 OR org_id IS NOT ({owner})
                    OR subject_id <> ?6 OR code <> ?8 OR params <> ?9 OR reasons <> ?10)"
        ),
        params![
            dedupe_key,
            severity,
            text.title,
            text.detail,
            subject_kind,
            subject_id,
            refresh_now,
            text.code,
            params_json,
            reasons_json
        ],
    )?;
    if updated == 1 {
        return Ok(false);
    }
    // The owner is stamped HERE, in the one statement every alert is born in,
    // rather than by the two dozen raisers: none of them has to remember it,
    // and a new raiser about an array cannot forget it (`alert_owner_sql`).
    let inserted = conn.execute(
        &format!(
            "INSERT OR IGNORE INTO nas_alerts
                (alert_id, severity, subject_kind, subject_id, title, detail, raised_at, dedupe_key,
                 org_id, code, params, reasons)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, {}, ?9, ?10, ?11)",
            alert_owner_sql("?3", "?4")
        ),
        params![
            uuid::Uuid::now_v7().to_string(),
            severity,
            subject_kind,
            subject_id,
            text.title,
            text.detail,
            now(),
            dedupe_key,
            text.code,
            params_json,
            reasons_json
        ],
    )?;
    Ok(inserted == 1)
}

/// `raise_coded_alert` with no code: English only, shown by the screens as
/// an uncoded alert (a generic translated title, the text as its tooltip).
/// For tests that only care about the row's lifecycle, and compiled for
/// nothing else: a production raiser has to word its alert as a code.
#[cfg(test)]
pub fn raise_alert(
    pool: &DbPool,
    dedupe_key: &str,
    severity: &str,
    subject_kind: &str,
    subject_id: &str,
    title: &str,
    detail: &str,
) -> Result<bool> {
    raise_coded_alert(pool, dedupe_key, severity, subject_kind, subject_id, &AlertText::new("", title, detail))
}

pub fn resolve_alert(pool: &DbPool, dedupe_key: &str) -> Result<()> {
    // Read first, and only then take a write connection. `evaluate_rows` calls
    // this for every healthy target on every 20 s tick — on a node with twenty
    // targets and no alerts at all, that used to be twenty SQLite write
    // transactions a minute for nothing, forever. The read is a keyed lookup
    // and the write only happens when there is something to close.
    let open: bool = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM nas_alerts WHERE dedupe_key = ?1 AND resolved_at IS NULL)",
            params![dedupe_key],
            |row| row.get(0),
        )?
    };
    if !open {
        return Ok(());
    }
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_alerts SET resolved_at = ?2 WHERE dedupe_key = ?1 AND resolved_at IS NULL",
        params![dedupe_key, now()],
    )?;
    Ok(())
}

/// The dedupe keys of the OPEN alerts whose key matches the SQL `LIKE`
/// pattern `like`. For a raiser that owns a family of per-subject keys and
/// has to close the members it no longer raises — including those of a
/// subject that has since disappeared, which it cannot name any more.
pub fn open_alert_keys_like(pool: &DbPool, like: &str) -> Result<Vec<String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT dedupe_key FROM nas_alerts WHERE resolved_at IS NULL AND dedupe_key LIKE ?1",
    )?;
    let rows = stmt
        .query_map(params![like], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The severity of the OPEN alert under `dedupe_key`, `None` when none is
/// open. The disk health alert moves FROM this rather than from a grade held
/// in memory, which a restart or a failed write leaves out of step with it.
pub fn open_alert_severity(pool: &DbPool, dedupe_key: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT severity FROM nas_alerts WHERE dedupe_key = ?1 AND resolved_at IS NULL",
            params![dedupe_key],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn ack_alert(pool: &DbPool, alert_id: &str) -> Result<bool> {
    let conn = write(pool)?;
    let n = conn.execute(
        "UPDATE nas_alerts SET acked_at = ?2 WHERE alert_id = ?1 AND acked_at IS NULL",
        params![alert_id, now()],
    )?;
    Ok(n == 1)
}

pub fn list_alerts(pool: &DbPool, include_acked: bool) -> Result<Vec<NasAlert>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let sql = format!(
        "SELECT {ALERT_COLUMNS} FROM nas_alerts
         WHERE resolved_at IS NULL {}
         ORDER BY raised_at DESC LIMIT 500",
        if include_acked { "" } else { "AND acked_at IS NULL" }
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], alert_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `list_alerts` as one organisation may see it (migration 18). The unscoped
/// `list_alerts` stays for the node's own loops, which act on every array.
pub fn list_alerts_for_org(pool: &DbPool, org_id: &str, include_acked: bool) -> Result<Vec<NasAlert>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let sql = format!(
        "SELECT {ALERT_COLUMNS} FROM nas_alerts
         WHERE resolved_at IS NULL AND {VISIBLE_TO_ORG_SQL} {}
         ORDER BY raised_at DESC LIMIT 500",
        if include_acked { "" } else { "AND acked_at IS NULL" }
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![org_id], alert_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `ack_alert` limited to the alerts `org_id` may see: another tenant's alert
/// is "not found" to it, exactly like an id that does not exist, so the
/// answer confirms nothing about it.
pub fn ack_alert_for_org(pool: &DbPool, org_id: &str, alert_id: &str) -> Result<bool> {
    let conn = write(pool)?;
    let n = conn.execute(
        &format!(
            "UPDATE nas_alerts SET acked_at = ?3
              WHERE alert_id = ?2 AND acked_at IS NULL AND {VISIBLE_TO_ORG_SQL}"
        ),
        params![org_id, alert_id, now()],
    )?;
    Ok(n == 1)
}

pub fn alerts_for_subject(pool: &DbPool, kind: &str, subject_id: &str) -> Result<Vec<NasAlert>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ALERT_COLUMNS} FROM nas_alerts
         WHERE subject_kind = ?1 AND subject_id = ?2 ORDER BY raised_at DESC LIMIT 50"
    ))?;
    let rows = stmt
        .query_map(params![kind, subject_id], alert_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Open, unacknowledged NODE-WIDE alerts (`org_id IS NULL`: disks, pools,
/// the node's own channel — a target's alert is its organisation's since
/// migration 20). This is the figure the node publishes in its fleet summary,
/// which lands in the instance's `addon_config` and is read by every tenant
/// of every node: a count that included a tenant's own alerts would tell the
/// others that it has some. Each tenant's own alerts are added at read time
/// (`count_open_alerts_for_org`).
pub fn count_open_node_alerts(pool: &DbPool) -> Result<u32> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM nas_alerts
          WHERE resolved_at IS NULL AND acked_at IS NULL AND org_id IS NULL",
        [],
        |r| r.get::<_, i64>(0),
    )? as u32)
}

/// Open, unacknowledged alerts one organisation may see — exactly the rows
/// `list_alerts_for_org(pool, org_id, false)` lists (the same visibility
/// clause), so a badge and the list under it never disagree.
pub fn count_open_alerts_for_org(pool: &DbPool, org_id: &str) -> Result<u32> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM nas_alerts
              WHERE resolved_at IS NULL AND acked_at IS NULL AND {VISIBLE_TO_ORG_SQL}"
        ),
        params![org_id],
        |r| r.get::<_, i64>(0),
    )? as u32)
}

// ----- jobs --------------------------------------------------------------------

fn job_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NasJob> {
    let log: String = r.get(9)?;
    Ok(NasJob {
        job_id: r.get(0)?,
        kind: r.get(1)?,
        subject: r.get(2)?,
        status: r.get(3)?,
        progress_pct: r.get::<_, Option<i64>>(4)?.map(|v| v.clamp(0, 100) as u8),
        started_by: r.get(5)?,
        started_at: r.get(6)?,
        finished_at: r.get(7)?,
        error: r.get(8)?,
        log: if log.is_empty() {
            Vec::new()
        } else {
            log.lines().map(str::to_string).collect()
        },
        // Stored subjects are what the job was spawned on; whether a shown
        // name is only remembered is decided on the way out (`name_jobs`).
        subject_last_known: false,
    })
}

const JOB_COLUMNS: &str = "job_id, kind, subject, status, progress_pct, started_by, started_at, \
                           finished_at, error, log";

/// Whether an array carries a parity operation nobody has resolved. `?1` is the
/// array id.
///
/// A SUCCESSFUL REPAIR SUPERSEDES the operations that preceded it, and that is
/// the whole reason this is one string rather than four copies of a `state =
/// 'needs_attention'` test. A scrub that finds errors closes `needs_attention`
/// and blocks every later sync, scrub, mover and add; `snapraid fix` is what
/// makes the array whole again, so after it succeeds those rows no longer
/// describe the array. They are NOT rewritten — a failed scrub happened, and
/// the history says so, and the history decoder is entitled to expect that a
/// row which carries no validated result reads `needs_attention`. What changes
/// is only whether they still hold the array.
///
/// The ordering compares `(created_at, operation_id)` as a pair because
/// `created_at` has second resolution: an operation id is a uuid v7, so the
/// pair is total even for two rows written in the same second.
///
/// A SUCCESSFUL SYNC OR SCRUB SUPERSEDES the parity runs that preceded it, and
/// a successful repair supersedes ONLY those (W5, W6 and W8 of the fourth
/// review). Three separate defects lived in the single `f.kind = 'fix'` clause
/// this replaces:
///
/// * Only a `fix` cleared a parity row, and a repair cannot always succeed. A
///   scrub marks bad blocks; `-e fix` writes back only those in files unchanged
///   since the last Sync, so when the marked blocks belong to files the users
///   have rewritten the repair reports `error_recovered:0` (MEASURED on rig11,
///   `x2`: 391 marked, 0 written) and its row is `failed`. The cure it names is
///   a Sync — and a Sync could not start, because the scrub row held the array.
///   Nothing could ever clear it.
/// * A Sync or Scrub the helper judged `Failed` left the same permanent state
///   with no repair evidence at all, so the product offered the admin nothing.
/// * And a repair cleared EVERY unresolved kind, so repairing scrub-marked
///   blocks silently declared a failed add-disk or a half-finished disk
///   replacement resolved.
///
/// A completed Sync recomputes parity for the data as it now is, and a full
/// Scrub that ends clean is positive evidence that parity matches it, so either
/// one supersedes an earlier parity run that ended without success. What that
/// does NOT do is hide the fault: `unresolved_parity_run` reads the HISTORY, so
/// a scrub that found errors goes on arming the repair until a repair succeeds.
///
/// A SUCCESSFUL MOVER SUPERSEDES the movers that preceded it, too. A mover run
/// that stopped with something unresolved — a record in flight, a record it
/// could neither finish nor reverse, a failed coupled Sync, a lost job — is
/// what the NEXT mover run resolves: it finishes or reverses the record,
/// closes the earlier run into the stuck history and syncs. Requiring a
/// `fix` for it would be a wedge, because the helper refuses a fix while that
/// run is open and an array without parity cannot run one at all.
///
/// AN INTERRUPTED SYNC OR SCRUB IS NOT UNRESOLVED (D2 of the second review).
/// A `sync` or `scrub` row closed `needs_attention` with NO result is one whose
/// process never reported: core restarted under it (`fail_orphaned_jobs`) or
/// the helper died. The helper closes such a run as interrupted and marks
/// parity out of date; a Scrub changed nothing. Nothing about it needs a
/// repair, and treating it as unresolved blocked the very Sync that resolves
/// it while the product offered `fix` instead — which reverts users' files.
/// A row `fail_orphaned_jobs` closed is interrupted too, even when its body
/// had recorded a result before core went away: the job never finished, and
/// the helper's journal, not this row, knows how the run ended. An interrupted
/// repair stays unresolved: it may have rewritten part of a disk, and the
/// repair is what can be asked again.
///
/// WHICH SUCCEEDED RUN SETTLES WHICH ROW, and why it is not any-to-any inside
/// the parity kinds (the fourth review's F2; the any-to-any version let a Sync
/// close a Scrub that had found errors and an interrupted repair, leaving an
/// array reading `active` with its fault still there and a Repair button that
/// could only answer "nothing repaired" until the row aged out of the history):
///
/// * a `mover` row -> a succeeded `mover`. It is the next run that finishes or
///   reverses the record in flight.
/// * a `sync` row -> a succeeded `sync`. Only a Sync makes parity current; a
///   repair writes data back from the OLD checkpoint and a scrub writes none.
/// * a `fix` row -> a succeeded `fix` or a succeeded full `scrub`. A repair
///   that was interrupted may have rewritten part of a disk, so a Sync is no
///   evidence about it at all - while a clean FULL scrub (the only scrub this
///   product runs, `-p full`) is positive evidence that nothing is left to
///   repair.
/// * a `scrub` row that counted DATA errors -> a succeeded `fix` or a later
///   clean `scrub`, never a Sync. MEASURED on rig11 (snapraid 13.0-1, probe
///   `scratchpad/f1probe/run3.sh` cases f1/f2/f3): a Sync leaves such a block
///   exactly as it was - the file reads `equal`, no parity is written for it,
///   and `-e fix` still recovers it afterwards. So a Sync settles nothing about
///   it, and saying it did would hide a fault that is still repairable.
/// * a `scrub` row with FILE errors only -> also a succeeded `sync`, because
///   that is the only cure the product has for it: `-e fix` writes nothing at
///   all for unreadable or missing files (measured, probe f4:
///   `error:0 recovered:0`), while a Sync removes them from the content file
///   and the next scrub then comes back clean. Without this the array would
///   have no action left that could clear the row.
///
/// So every row keeps at least one reachable way out, and none is closed by a
/// run that did not address it.
macro_rules! orphaned_operation_error {
    () => {
        "Utracono nadzór core; wymagany odczyt journala roota"
    };
}
const ORPHANED_OPERATION_ERROR: &str = orphaned_operation_error!();

macro_rules! unresolved_elastic_operation {
    ($kinds:literal) => {
        concat!(
            "EXISTS(
    SELECT 1 FROM nas_elastic_operations o
    WHERE o.array_id = ?1
      AND o.kind IN (",
            $kinds,
            ")
      AND o.state = 'needs_attention'
      AND NOT (o.kind IN ('sync','scrub') AND (o.result_json IS NULL OR o.error = '",
            orphaned_operation_error!(),
            "'))
      AND NOT EXISTS(
        SELECT 1 FROM nas_elastic_operations f
        WHERE f.array_id = o.array_id AND f.state = 'succeeded'
          AND ((o.kind = 'mover' AND f.kind = 'mover')
               OR (o.kind = 'sync' AND f.kind = 'sync')
               OR (o.kind = 'fix' AND f.kind IN ('fix','scrub'))
               OR (o.kind = 'scrub'
                   AND (f.kind IN ('fix','scrub')
                        OR (f.kind = 'sync'
                            AND COALESCE(CASE WHEN json_valid(o.result_json)
                                              THEN json_extract(o.result_json,'$.run.errors_data')
                                         END, 0) = 0))))
          AND (f.created_at, f.operation_id) > (o.created_at, o.operation_id)))"
        )
    };
}
const UNRESOLVED_ELASTIC_OPERATION: &str =
    unresolved_elastic_operation!("'sync','scrub','mover','fix','add_disk','replace_disk'");
/// The same, for every kind but the mover's own runs.
const UNRESOLVED_NON_MOVER_OPERATION: &str =
    unresolved_elastic_operation!("'sync','scrub','fix','add_disk','replace_disk'");
/// Whether the array's needs_attention came from a mover run: its newest
/// operation of any kind is a mover closed `needs_attention`. The array's
/// `create` never counts as newer: it precedes every mover by definition, and
/// its id need not be a uuid v7, so within one second the pair ordering could
/// otherwise put it after the run. Nor does a `failed` row (W3 of the second
/// review): every Elastic command takes the node lock without waiting, so a
/// run refused as busy — or a Sync refused before it started — is written
/// `failed` with nothing having run, and one such collision must not turn
/// the settling off for good.
const ATTENTION_FROM_MOVER: &str = "EXISTS(
    SELECT 1 FROM nas_elastic_operations l
    WHERE l.array_id = ?1 AND l.kind = 'mover' AND l.state = 'needs_attention'
      AND NOT EXISTS(
        SELECT 1 FROM nas_elastic_operations n
        WHERE n.array_id = l.array_id AND n.kind <> 'create' AND n.state <> 'failed'
          AND (n.created_at, n.operation_id) > (l.created_at, l.operation_id)))";

/// Whether a mover run may start on an array in `state`: an active array with
/// nothing unresolved, or one whose every unresolved operation is a mover run —
/// the run settles those — and whose needs_attention, if any, is theirs.
/// Returns (may start, something unresolved it would settle).
fn mover_admission(conn: &Connection, array_id: &str, state: &str) -> Result<(bool, bool)> {
    let flag = |sql: &str| -> Result<bool> {
        Ok(conn.query_row(&format!("SELECT {sql}"), params![array_id], |r| r.get(0))?)
    };
    let unresolved = flag(UNRESOLVED_ELASTIC_OPERATION)?;
    let other = flag(UNRESOLVED_NON_MOVER_OPERATION)?;
    let state_admits = state == "active" || (state == "needs_attention" && flag(ATTENTION_FROM_MOVER)?);
    Ok((state_admits && !other, unresolved && !other && state_admits))
}

/// The unresolved operations a Sync cannot settle: everything that is not a
/// parity run of its own kind. A Sync writes parity for the data as it is, so
/// an earlier Sync, Scrub or repair that ended without success is exactly what
/// it answers; a half-finished add or replacement, or a mover run with a file
/// in flight, is not.
const UNRESOLVED_OUTSIDE_PARITY: &str =
    unresolved_elastic_operation!("'mover','add_disk','replace_disk'");

/// Whether a Sync or a full Scrub may start on an array in `state`.
///
/// W5/W6 of the fourth review. Before this, both required `active` with
/// nothing unresolved — and the rows they are the cure for are exactly the ones
/// that take an array out of `active`. A repair whose own detail says "run a
/// Sync" left the Sync refused, the mover refused with it, and the cache
/// filling to ENOSPC with parity frozen for good.
///
/// So they may also start on an array whose only unresolved operations are
/// parity runs. Nothing else is admitted: a mover record in flight, a
/// half-finished add or a half-finished replacement are states a Sync would
/// write parity over rather than settle.
fn parity_admission(conn: &Connection, array_id: &str, state: &str) -> Result<bool> {
    let flag = |sql: &str| -> Result<bool> {
        Ok(conn.query_row(&format!("SELECT {sql}"), params![array_id], |r| r.get(0))?)
    };
    if !matches!(state, "active" | "needs_attention") {
        return Ok(false);
    }
    Ok(!flag(UNRESOLVED_OUTSIDE_PARITY)?)
}

/// Mover runs closed `needs_attention` since the last one that succeeded,
/// newest first. `failed` rows (refused before anything ran) and running ones
/// are passed over; any other kind of operation is not a mover run.
fn mover_failed_runs(conn: &Connection, array_id: &str) -> Result<u32> {
    let mut statement = conn.prepare(
        "SELECT state FROM nas_elastic_operations WHERE array_id=?1 AND kind='mover'
         AND state IN ('succeeded','needs_attention')
         ORDER BY created_at DESC, operation_id DESC LIMIT 32",
    )?;
    let states = statement
        .query_map(params![array_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(states.iter().take_while(|state| *state == "needs_attention").count() as u32)
}

/// Whether an operation of this array is running right now. The scheduler asks
/// before it pays for a cache probe: while one runs, the helper holds the
/// array's lock, and the only possible answer would be a refusal.
pub fn elastic_operation_running(pool: &DbPool, array_id: &str) -> Result<bool> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM nas_elastic_operations WHERE array_id = ?1 AND state = 'running')",
        params![array_id],
        |r| r.get(0),
    )?)
}

pub fn insert_job(pool: &DbPool, job: &NasJob, intent: Option<&ElasticJobIntent>) -> Result<()> {
    insert_job_owned(pool, job, intent, None)
}

/// `insert_job` with the row's owner written BY THE INSERT itself (migration
/// 18's `org_id`), so the row is never visible to another organisation, not
/// even between two statements.
///
/// ONE RULE FOR THE OWNER: an Elastic job's owner is its array's
/// organisation, stamped in this same transaction (`stamp_elastic_job_org`),
/// and a caller may not name another one — an explicit owner on an Elastic
/// kind is refused rather than letting two sources disagree. Every other job
/// is node-wide (NULL) unless `owner` says whose it is.
pub fn insert_job_owned(
    pool: &DbPool,
    job: &NasJob,
    intent: Option<&ElasticJobIntent>,
    owner: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(owner.is_none() || !job.kind.starts_with("elastic_"),
        "Zadanie Elastic należy do organizacji swojej macierzy; jawny właściciel jest odrzucany");
    anyhow::ensure!(owner.is_none_or(|org| !org.is_empty()),
        "Pusty identyfikator organizacji nie jest właścicielem");
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if intent.is_some() {
        let closing:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM nas_settings WHERE key='elastic_teardown_started')",
            [],|r| r.get(0))?;
        anyhow::ensure!(!closing,"Rozpoczęto usuwanie instancji; odmowa nowej intencji Elastic");
    }
    // A second self-test on a disk that is already running one would ABORT the
    // first (ATA, SPC and NVMe all behave this way), and the long test spans
    // many ticks of a daily short schedule. The refusal lives HERE, not at the
    // callers: the scheduler's two passes, the manual start from the disk
    // detail view and two rapid clicks on it all arrive through this
    // transaction, and only here are the check and the insert one atomic step.
    // Scoped to `smart_test` because no other kind serialises on its subject
    // this way — a pool takes two scrubs, a dataset two snapshots.
    if job.kind == "smart_test" {
        let running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM nas_jobs
             WHERE kind = 'smart_test' AND subject = ?1 AND status = 'running')",
            params![job.subject],
            |r| r.get(0),
        )?;
        anyhow::ensure!(!running, "Na tym dysku trwa już autotest SMART; odmowa drugiego");
    }
    tx.execute(
        "INSERT INTO nas_jobs (job_id, kind, subject, status, progress_pct, started_by,
                               started_at, log, org_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            job.job_id,
            job.kind,
            job.subject,
            job.status,
            job.progress_pct.map(i64::from),
            job.started_by,
            job.started_at,
            job.log.join("\n"),
            owner
        ],
    )?;
    if let Some(intent) = intent {
        let (array_id, operation_id, kind, request) = match intent {
            ElasticJobIntent::Create(spec) => {
                spec.validate()?;
                anyhow::ensure!(job.kind == "elastic_create" && job.subject == spec.name,
                    "Zadanie nie odpowiada intencji create");
                tx.execute("INSERT INTO nas_elastic_arrays
                    (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
                    VALUES (?1,?2,?3,?4,?5,'creating','',?6,?6)",
                    params![spec.array_id,spec.owner.org_id,spec.owner.addon_id,spec.name,
                        spec.filesystem.as_str(),job.started_at])?;
                insert_elastic_disks(&tx, spec)?;
                // In the transaction that writes the array row, so there is no
                // array without its default scrub for the scheduler to find.
                insert_default_scrub_schedule(&tx, &spec.array_id)?;
                (&spec.array_id, &spec.operation_id, "create", serde_json::to_string(spec)?)
            }
            ElasticJobIntent::Restore { owner, array_id, operation_id } => {
                let spec = elastic_spec(&tx, owner, array_id)?;
                anyhow::ensure!(job.kind == "elastic_restore" && job.subject == spec.name,
                    "Zadanie nie odpowiada intencji restore");
                anyhow::ensure!(uuid::Uuid::parse_str(operation_id)?.to_string() == *operation_id,
                    "Niekanoniczny identyfikator operacji");
                (array_id, operation_id, "restore", serde_json::to_string(&tentanas_helper::HelperCommand::ElasticRestore {
                    array_id: array_id.clone(), owner: owner.clone() })?)
            }
            ElasticJobIntent::Snapraid { owner, array_id, operation_id, kind, acknowledge_parity_fault } => {
                let spec = elastic_spec(&tx, owner, array_id)?;
                let action = super::elastic::snapraid_kind(kind);
                anyhow::ensure!(job.kind == format!("elastic_{action}") && job.subject == spec.name,
                    "Zadanie nie odpowiada intencji SnapRAID");
                anyhow::ensure!(!spec.parity.is_empty(), "Macierz bez parity nie wykonuje SnapRAID");
                if let Some(disk) = super::elastic::snapraid_disk(kind) {
                    // The repair's disk is checked against the array HERE, in
                    // the transaction that reserves the operation: a row that
                    // could only ever produce a refusal must not be written,
                    // and the helper's own refusal would arrive as a failed job
                    // with no sentence the admin can act on.
                    anyhow::ensure!(
                        spec.data.iter().enumerate().any(|(index, _)|
                            tentanas_helper::elastic::data_branch_name(index + 1) == disk),
                        "Macierz nie ma dysku danych '{disk}'");
                }
                let state: String = tx.query_row("SELECT state FROM nas_elastic_arrays WHERE array_id=?1",
                    params![array_id], |r| r.get(0))?;
                let unresolved: bool = tx.query_row(
                    &format!("SELECT {UNRESOLVED_ELASTIC_OPERATION}"),
                    params![array_id], |r| r.get(0))?;
                // A REPAIR is the one maintenance operation an array that needs
                // attention may start, because it is the one that RESOLVES that
                // state: a scrub which found errors leaves exactly this row, and
                // refusing the repair here would make the unresolved operation
                // permanent and the array unrepairable through the product.
                let _ = unresolved;
                anyhow::ensure!(
                    if super::elastic::snapraid_disk(kind).is_some() {
                        matches!(state.as_str(), "active" | "needs_attention")
                    } else {
                        // A Sync or a full Scrub is what settles a parity run
                        // that ended without success, so it is admitted on the
                        // array such a run left behind (`parity_admission`).
                        parity_admission(&tx, array_id, &state)?
                    },
                    "Macierz ma niepotwierdzoną operację; brak ponowienia");
                tentanas_helper::elastic::validate_elastic_uuid(operation_id)?;
                // Only a Sync may carry the acknowledgement; a Scrub or a Fix
                // that claimed one would be a request the helper never reads.
                anyhow::ensure!(
                    acknowledge_parity_fault.is_none() || *kind == tentanas_helper::elastic::ElasticSnapraidKind::Sync,
                    "Potwierdzenie błędu parity dotyczy tylko Sync"
                );
                if let Some(fault) = acknowledge_parity_fault {
                    tentanas_helper::elastic::validate_elastic_uuid(fault)?;
                }
                let command = super::elastic::snapraid_command(
                    owner,
                    array_id,
                    operation_id,
                    kind,
                    acknowledge_parity_fault.as_deref(),
                );
                (array_id, operation_id, action, serde_json::to_string(&command)?)
            }
            ElasticJobIntent::AddDisk { owner, array_id, operation_id, disk } => {
                let spec = elastic_spec(&tx, owner, array_id)?;
                anyhow::ensure!(job.kind == "elastic_add_disk" && job.subject == spec.name,
                    "Zadanie nie odpowiada intencji dodania dysku");
                tentanas_helper::elastic::validate_elastic_uuid(operation_id)?;
                // The array the add would PRODUCE has to be a legal array: the
                // 32-device ceiling and the identity uniqueness are checked on
                // it, not on the disk alone.
                super::elastic::spec_with_added_disk(&spec, disk)?;
                // The RESERVATION, in the transaction that opens the operation
                // and before the helper formats anything: the member row cannot
                // be written yet — `elastic_spec` cross-checks it against the
                // array's persisted intention, which is still the array without
                // this disk — so the uniqueness rules the row would enforce are
                // enforced here by hand instead. It also answers whether this
                // is the RETRY of an add that stopped part-way.
                let retry = reserve_added_disk(&tx, array_id, disk)?;
                let state: String = tx.query_row("SELECT state FROM nas_elastic_arrays WHERE array_id=?1",
                    params![array_id], |r| r.get(0))?;
                let unresolved: bool = tx.query_row(
                    &format!("SELECT {UNRESOLVED_ELASTIC_OPERATION}"),
                    params![array_id], |r| r.get(0))?;
                // A RETRY is admitted on an array that needs attention, because
                // the operation needing attention is the add itself: the helper
                // recorded the slot in its journal before it formatted
                // anything, so repeating the command is what finishes the work
                // — and refusing it here would leave a disk half-joined with no
                // way through the product to either finish or undo it.
                anyhow::ensure!(
                    if retry {
                        matches!(state.as_str(), "active" | "needs_attention")
                    } else {
                        state == "active" && !unresolved
                    },
                    "Macierz ma niepotwierdzoną operację; brak ponowienia");
                // The attempt this one resumes keeps holding the array until
                // the resume SUCCEEDS (`finish_elastic_add_disk`): a resume
                // the helper refuses must leave the add pinned, or the disk
                // would sit half-joined with no way through the product to
                // finish or undo it.
                let command = super::elastic::add_disk_command(owner, array_id, operation_id, disk);
                (array_id, operation_id, "add_disk", serde_json::to_string(&command)?)
            }
            // THE UNDO of an unfinished add (K7). It names the pinned add's
            // own identity — nothing else may be undone, and only the
            // filesystem that add gave the disk is ever erased — and it is
            // admitted on an array that needs attention, because the add
            // needing attention is what it resolves. Whether the disk may
            // still be taken out is the helper's live read of the union, not
            // anything this database knows.
            ElasticJobIntent::AddDiskAbort { owner, array_id, operation_id, disk } => {
                let spec = elastic_spec(&tx, owner, array_id)?;
                anyhow::ensure!(job.kind == "elastic_add_disk_abort" && job.subject == spec.name,
                    "Zadanie nie odpowiada intencji wycofania dodania dysku");
                tentanas_helper::elastic::validate_elastic_uuid(operation_id)?;
                anyhow::ensure!(pinned_add(&tx, array_id)?.as_ref() == Some(disk),
                    "Macierz nie ma niedokończonego dodania tego dysku");
                let state: String = tx.query_row("SELECT state FROM nas_elastic_arrays WHERE array_id=?1",
                    params![array_id], |r| r.get(0))?;
                anyhow::ensure!(matches!(state.as_str(), "active" | "needs_attention"),
                    "Wycofanie dodania dysku wymaga macierzy aktywnej albo wymagającej uwagi");
                let command = super::elastic::add_disk_abort_command(owner, array_id, operation_id, disk);
                (array_id, operation_id, "add_disk_abort", serde_json::to_string(&command)?)
            }
            // DISK REPLACEMENT IS WITHDRAWN (round 4, owner's decision), and
            // this arm is the last gate: NO path may create a `replace_disk`
            // operation row, because a failed one wedged the array (only a
            // SUCCEEDED `fix` cleared it, and the helper could not run the
            // command at all). The dispatch handler already refuses before it
            // reaches a store call; this refusal exists so a future caller —
            // the scheduler, a retry path, a restored approval — cannot open
            // one behind the handler's back. The variant and its command
            // builder stay, dormant, for the task that finishes the feature;
            // `dispatch::tentanas::elastic_replace_disk` carries the list of
            // what that task still has to solve.
            ElasticJobIntent::ReplaceDisk { .. } => {
                anyhow::bail!(
                    "Wymiana dysku nie jest udostępniona w tej wersji; operacja nie została otwarta"
                );
            }
            ElasticJobIntent::Dissolve { owner, array_id, operation_id } => {
                let spec = elastic_spec(&tx, owner, array_id)?;
                anyhow::ensure!(job.kind == "elastic_destroy" && job.subject == spec.name,
                    "Zadanie nie odpowiada intencji rozwiązania");
                tentanas_helper::elastic::validate_elastic_uuid(operation_id)?;
                let command = super::elastic::dissolve_command(owner, array_id, operation_id);
                (array_id, operation_id, "dissolve", serde_json::to_string(&command)?)
            }
            ElasticJobIntent::Mover { owner, array_id, operation_id, resume_operation_id, rules, coupled_sync } => {
                let spec = elastic_spec(&tx, owner, array_id)?;
                anyhow::ensure!(job.kind == "elastic_mover" && job.subject == spec.name,
                    "Zadanie nie odpowiada intencji movera");
                // DELIBERATELY no parity guard, unlike SnapRAID: moving files
                // off the cache is worth doing on an array with no parity at
                // all, and the helper simply skips the coupled sync there.
                let state: String = tx.query_row("SELECT state FROM nas_elastic_arrays WHERE array_id=?1",
                    params![array_id], |r| r.get(0))?;
                // A stuck sync or scrub blocks a mover too: an unresolved parity
                // operation means nothing knows what parity currently covers,
                // and a run would move more bytes out of the cache and then
                // sync on top of that unknown. An unresolved MOVER does not: the
                // next run is what finishes or reverses what it left.
                let (admitted, _) = mover_admission(&tx, array_id, &state)?;
                anyhow::ensure!(admitted, "Macierz ma niepotwierdzoną operację; brak ponowienia");
                tentanas_helper::elastic::validate_elastic_uuid(operation_id)?;
                tentanas_helper::elastic::validate_elastic_uuid(resume_operation_id)?;
                // The same distinctness `validate_observation` demands of the
                // answer, enforced on the INTENT: a row that could only ever
                // produce a refused answer never reaches the journal.
                anyhow::ensure!(resume_operation_id != operation_id
                    && *resume_operation_id != spec.operation_id && *operation_id != spec.operation_id,
                    "Resume movera musi mieć osobną operację");
                let command = super::elastic::mover_command(owner, array_id, operation_id,
                    resume_operation_id, rules, *coupled_sync);
                (array_id, operation_id, "mover", serde_json::to_string(&command)?)
            }
        };
        anyhow::ensure!(request.len() < 16 * 1024, "Intencja Elastic przekracza limit");
        tx.execute("INSERT INTO nas_elastic_operations
            (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
            VALUES (?1,?2,?3,?4,'running',?5,'',?6)",
            params![operation_id,array_id,job.job_id,kind,request,job.started_at])?;
    }
    // After the intent: a create's array row is written above, in this tx.
    stamp_elastic_job_org(&tx, &job.job_id, &job.kind, &job.subject)?;
    tx.commit()?;
    Ok(())
}

pub fn append_job_log(pool: &DbPool, job_id: &str, line: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_jobs SET log = CASE WHEN log = '' THEN ?2 ELSE log || char(10) || ?2 END
         WHERE job_id = ?1",
        params![job_id, line],
    )?;
    Ok(())
}

pub fn set_job_progress(pool: &DbPool, job_id: &str, status: &str, pct: Option<u8>) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_jobs SET status = ?2, progress_pct = ?3 WHERE job_id = ?1",
        params![job_id, status, pct.map(i64::from)],
    )?;
    Ok(())
}

/// Whether a job failed because the helper found another Elastic command
/// holding the lock (`tentanas_helper::elastic::ELASTIC_BUSY`). The age probe
/// walks a whole cache under that lock, so a manual Sync or mover started
/// meanwhile meets it; such a refusal ran nothing and must not leave an
/// unresolved operation behind.
/// Whether the job's error means NOTHING RAN on the array: another Elastic
/// command held the helper's lock, or the node refused to talk to a helper of
/// another build than this core (`broker::HELPER_VERSION_MARKER`).
///
/// Both close the operation row as `failed` and leave the array untouched,
/// which is the difference between a refusal and a fault. The version refusal
/// matters most for the unattended paths: during the window between a core
/// upgrade and the admin re-running provisioning, every cadence tick is
/// refused, and a refusal that parked `needs_attention` on the array would
/// take out its Sync, its Scrub and its mover as well.
fn nothing_ran(error: Option<&str>) -> bool {
    error.is_some_and(|error| {
        error.contains(tentanas_helper::elastic::ELASTIC_BUSY)
            || error.contains(super::broker::HELPER_VERSION_MARKER)
    })
}

pub fn finish_job(pool: &DbPool, job_id: &str, status: &str, error: Option<&str>) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let at = now();
    let maintenance = snapraid_operation(&tx, job_id)?;
    // A mover job closes its own operation row here, from the result its body
    // recorded — without this the row would stay 'running' forever and the
    // unique running-operation index would lock the array out of every later
    // operation.
    let mover = if maintenance.is_none() { mover_operation(&tx, job_id)? } else { None };
    let requires_row = maintenance.is_some() || mover.is_some();
    let mut final_status = status.to_string();
    let mut final_error = error.map(str::to_string);
    if let Some((operation_id, spec, kind, candidate)) = maintenance {
        let validated = candidate
            .as_deref()
            .ok_or_else(|| anyhow!("Brak wyniku operacji SnapRAID"))
            .and_then(|json| {
                anyhow::ensure!(json.len() < 64 * 1024, "Za duży wynik SnapRAID");
                let result: tentanas_helper::elastic::ElasticSnapraidResult =
                    serde_json::from_str(json)?;
                let answered =
                    super::elastic::answered_spec(&spec, pinned_add(&tx, &spec.array_id)?.as_ref(), &result.state)?;
                super::elastic::validate_snapraid_result(&answered, &operation_id, &kind, &result)?;
                Ok(result)
            });
        let (operation_state, update_array, array_state) = match validated {
            // Refused before it started: another Elastic command held the
            // lock, or the helper is not the build this core speaks to.
            // Nothing ran, so nothing needs attention; the run can be asked
            // again once the reason is gone.
            Err(_) if nothing_ran(final_error.as_deref()) => {
                final_status = "failed".into();
                ("failed", false, "active")
            }
            // A Sync that skipped files changing under it is not a fault: the
            // array serves on and parity waits for the next Sync.
            Ok(result)
                if status == "succeeded"
                    && matches!(
                        result.run.outcome,
                        tentanas_helper::elastic::ElasticSnapraidOutcome::Succeeded
                            | tentanas_helper::elastic::ElasticSnapraidOutcome::Partial
                    ) =>
            {
                ("succeeded", true, "active")
            }
            Ok(result)
                if status == "failed"
                    && result.run.outcome
                        == tentanas_helper::elastic::ElasticSnapraidOutcome::Refused =>
            {
                ("failed", false, "active")
            }
            // A REPAIR THAT WROTE NOTHING. It is not a success — the fault it
            // was started for is still there, and the scrub row that reported
            // it still holds the array — and it is not an unresolved operation
            // of its own either: an unresolved `fix` is cleared only by a
            // successful `fix`, and a repair cannot succeed until a scrub marks
            // blocks for it, while that scrub needs an array nothing holds. So
            // the row is `failed` and the ARRAY is left exactly as it was.
            Ok(result)
                if result.run.outcome
                    == tentanas_helper::elastic::ElasticSnapraidOutcome::NothingRepaired =>
            {
                final_status = "failed".into();
                if final_error.is_none() {
                    final_error = result.run.detail.clone();
                }
                ("failed", false, "active")
            }
            // INTERRUPTED, not broken (W-D of the third review): a Sync or a
            // Scrub that recorded no result had no verdict to record — the
            // helper died, the job timed out, a precondition refused it. The
            // operation row keeps that as `needs_attention` and reads as
            // interrupted, which blocks nothing; the ARRAY is left exactly as
            // it was, because a state of needs_attention would refuse the very
            // Sync the row says will finish it.
            Err(_) if candidate.is_none() && super::elastic::snapraid_disk(&kind).is_none() => {
                final_status = "failed".into();
                if final_error.is_none() {
                    final_error = Some("Brak wyniku operacji SnapRAID".into());
                }
                ("needs_attention", false, "active")
            }
            other => {
                final_status = "failed".into();
                if let Err(cause) = other {
                    final_error = Some(format!("Niepotwierdzony wynik SnapRAID: {cause}"));
                } else if final_error.is_none() {
                    final_error = Some("Niezgodny terminalny wynik SnapRAID".into());
                }
                ("needs_attention", true, "needs_attention")
            }
        };
        anyhow::ensure!(
            tx.execute(
                "UPDATE nas_elastic_operations SET state=?2,error=?3,finished_at=?4
            WHERE operation_id=?1 AND job_id=?5 AND state='running'",
                params![
                    operation_id,
                    operation_state,
                    final_error.as_deref().unwrap_or(""),
                    at,
                    job_id
                ]
            )? == 1,
            "Utracono running operację SnapRAID"
        );
        if update_array {
            anyhow::ensure!(
                tx.execute(
                    "UPDATE nas_elastic_arrays SET state=?2,state_detail=?3,updated_at=?4
                WHERE array_id=?1 AND org_id=?5 AND addon_id=?6",
                    params![
                        spec.array_id,
                        array_state,
                        final_error.as_deref().unwrap_or(""),
                        at,
                        spec.owner.org_id,
                        spec.owner.addon_id
                    ]
                )? == 1,
                "Utracono macierz operacji SnapRAID"
            );
        }
    }
    if let Some((operation_id, spec, candidate)) = mover {
        let validated = candidate
            .as_deref()
            .ok_or_else(|| anyhow!("Brak wyniku operacji movera"))
            .and_then(|json| {
                anyhow::ensure!(json.len() < 64 * 1024, "Za duży wynik movera");
                let result: tentanas_helper::elastic::ElasticMoverResult =
                    serde_json::from_str(json)?;
                super::elastic::validate_mover_result(&spec, &operation_id, &result)?;
                Ok(result)
            });
        // The union serves throughout every run; what differs is whether the
        // helper resolved everything. Only a Complete result — validated as a
        // `Ready` array, so without any Hold and with the union read-write —
        // marks the array active. A run that stopped with something unresolved
        // (a record neither finished nor reversed, a failed Sync) closes as
        // needs_attention and the array says so until an admin looks.
        let (operation_state, array_state) = match validated {
            Ok(result)
                if status == "succeeded"
                    && result.run.phase
                        == tentanas_helper::elastic::ElasticMoverPhase::Complete =>
            {
                ("succeeded", Some("active"))
            }
            // Refused before it started: the lock, or a helper of another
            // build (`nothing_ran`). The array is left exactly as it was.
            Err(_) if nothing_ran(final_error.as_deref()) => {
                final_status = "failed".into();
                ("failed", None)
            }
            other => {
                final_status = "failed".into();
                if let Err(cause) = other {
                    final_error = Some(format!("Niepotwierdzony wynik movera: {cause}"));
                } else if final_error.is_none() {
                    final_error = Some("Niezgodny terminalny wynik movera".into());
                }
                ("needs_attention", Some("needs_attention"))
            }
        };
        anyhow::ensure!(
            tx.execute(
                "UPDATE nas_elastic_operations SET state=?2,error=?3,finished_at=?4
            WHERE operation_id=?1 AND job_id=?5 AND state='running'",
                params![
                    operation_id,
                    operation_state,
                    final_error.as_deref().unwrap_or(""),
                    at,
                    job_id
                ]
            )? == 1,
            "Utracono running operację movera"
        );
        if let Some(array_state) = array_state {
            anyhow::ensure!(
                tx.execute(
                    "UPDATE nas_elastic_arrays SET state=?2,state_detail=?3,updated_at=?4
                WHERE array_id=?1 AND org_id=?5 AND addon_id=?6",
                    params![
                        spec.array_id,
                        array_state,
                        final_error.as_deref().unwrap_or(""),
                        at,
                        spec.owner.org_id,
                        spec.owner.addon_id
                    ]
                )? == 1,
                "Utracono macierz operacji movera"
            );
        }
    }
    let changed = tx.execute(
        "UPDATE nas_jobs SET status = ?2, error = ?3, finished_at = ?4,
                progress_pct = CASE WHEN ?2 = 'succeeded' THEN 100 ELSE progress_pct END
         WHERE job_id = ?1",
        params![job_id, final_status, final_error, at],
    )?;
    anyhow::ensure!(
        !requires_row || changed == 1,
        "Nie utrwalono końca dokładnie jednego joba SnapRAID"
    );
    tx.commit()?;
    anyhow::ensure!(
        status != "succeeded" || final_status == "succeeded",
        "Nie potwierdzono sukcesu zadania SnapRAID"
    );
    Ok(())
}

type SnapraidOperation = (
    String,
    ElasticCreateSpec,
    tentanas_helper::elastic::ElasticSnapraidKind,
    Option<String>,
);

fn snapraid_operation(conn: &Connection, job_id: &str) -> Result<Option<SnapraidOperation>> {
    let header: Option<(String,String,String,String,String,String,Option<String>,String,String)> = conn.query_row(
        "SELECT o.operation_id,o.array_id,o.kind,a.org_id,a.addon_id,o.request_json,o.result_json,j.kind,j.subject
         FROM nas_elastic_operations o JOIN nas_elastic_arrays a ON a.array_id=o.array_id
         JOIN nas_jobs j ON j.job_id=o.job_id
         WHERE o.job_id=?1 AND o.kind IN ('sync','scrub','fix') AND o.state='running'
         AND j.status IN ('queued','running')", params![job_id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional()?;
    let Some((
        operation_id,
        array_id,
        action,
        org_id,
        addon_id,
        request,
        candidate,
        job_kind,
        subject,
    )) = header
    else {
        let is_maintenance: bool = conn
            .query_row(
                "SELECT kind IN ('elastic_sync','elastic_scrub','elastic_fix')
                 FROM nas_jobs WHERE job_id=?1",
                params![job_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        anyhow::ensure!(
            !is_maintenance,
            "Brak dokładnie jednej running operacji SnapRAID"
        );
        return Ok(None);
    };
    let owner = ElasticOwner { org_id, addon_id };
    let spec = elastic_spec(conn, &owner, &array_id)?;
    tentanas_helper::elastic::validate_elastic_uuid(&operation_id)?;
    anyhow::ensure!(
        job_kind == format!("elastic_{action}")
            && subject == spec.name
            && request.len() < 16 * 1024,
        "Niezgodna intencja zadania SnapRAID"
    );
    let command: tentanas_helper::HelperCommand = serde_json::from_str(&request)?;
    // A repair's disk is part of its KIND, and the only record of it is the
    // stored request — so the kind is read back out of the command and then the
    // whole command is rebuilt from it and compared. A request whose disk had
    // been edited would fail that comparison exactly as a request whose array
    // had been.
    // The Sync's acknowledgement is part of the stored command the same
    // way: read back, then held to the rebuilt command like everything else.
    let acknowledged = match &command {
        tentanas_helper::HelperCommand::ElasticSync { acknowledge_parity_fault, .. } => acknowledge_parity_fault.clone(),
        _ => None,
    };
    let kind = match (action.as_str(), &command) {
        ("sync", _) => tentanas_helper::elastic::ElasticSnapraidKind::Sync,
        ("scrub", _) => tentanas_helper::elastic::ElasticSnapraidKind::Scrub,
        ("fix", tentanas_helper::HelperCommand::ElasticFix { disk, .. }) => {
            tentanas_helper::elastic::ElasticSnapraidKind::Fix { disk: disk.clone() }
        }
        _ => anyhow::bail!("Zmienione żądanie operacji SnapRAID"),
    };
    anyhow::ensure!(
        command == super::elastic::snapraid_command(&owner, &array_id, &operation_id, &kind, acknowledged.as_deref()),
        "Zmienione żądanie operacji SnapRAID"
    );
    Ok(Some((operation_id, spec, kind, candidate)))
}

pub fn record_snapraid_result(
    pool: &DbPool,
    owner: &ElasticOwner,
    operation_id: &str,
    result: &tentanas_helper::elastic::ElasticSnapraidResult,
) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let job_id: String = tx.query_row(
        "SELECT o.job_id FROM nas_elastic_operations o
        JOIN nas_elastic_arrays a ON a.array_id=o.array_id
        WHERE o.operation_id=?1 AND o.state='running' AND a.org_id=?2 AND a.addon_id=?3",
        params![operation_id, owner.org_id, owner.addon_id],
        |r| r.get(0),
    )?;
    let (stored_id, spec, kind, candidate) =
        snapraid_operation(&tx, &job_id)?.ok_or_else(|| anyhow!("Brak intencji SnapRAID"))?;
    anyhow::ensure!(
        stored_id == operation_id && candidate.is_none(),
        "Wynik operacji już zapisano"
    );
    let answered =
        super::elastic::answered_spec(&spec, pinned_add(&tx, &spec.array_id)?.as_ref(), &result.state)?;
    super::elastic::validate_snapraid_result(&answered, operation_id, &kind, result)?;
    let json = serde_json::to_string(result)?;
    anyhow::ensure!(json.len() < 64 * 1024, "Za duży wynik SnapRAID");
    anyhow::ensure!(tx.execute("UPDATE nas_elastic_operations SET result_json=?2 WHERE operation_id=?1 AND state='running' AND result_json IS NULL",
        params![operation_id,json])? == 1, "Nie zapisano kandydata SnapRAID");
    tx.commit()?;
    Ok(())
}

type MoverOperation = (String, ElasticCreateSpec, Option<String>);

/// The running mover operation of one job, with the result candidate its body
/// recorded. `None` for every job that is not a mover — but a job whose KIND
/// says mover and that has no such row is a contradiction, not a no-op.
fn mover_operation(conn: &Connection, job_id: &str) -> Result<Option<MoverOperation>> {
    let header: Option<(String, String, String, String, String, Option<String>, String, String)> = conn.query_row(
        "SELECT o.operation_id,o.array_id,a.org_id,a.addon_id,o.request_json,o.result_json,j.kind,j.subject
         FROM nas_elastic_operations o JOIN nas_elastic_arrays a ON a.array_id=o.array_id
         JOIN nas_jobs j ON j.job_id=o.job_id
         WHERE o.job_id=?1 AND o.kind='mover' AND o.state='running'
         AND j.status IN ('queued','running')", params![job_id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
    let Some((operation_id, array_id, org_id, addon_id, request, candidate, job_kind, subject)) =
        header
    else {
        let is_mover: bool = conn
            .query_row(
                "SELECT kind='elastic_mover' FROM nas_jobs WHERE job_id=?1",
                params![job_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        anyhow::ensure!(!is_mover, "Brak dokładnie jednej running operacji movera");
        return Ok(None);
    };
    let owner = ElasticOwner { org_id, addon_id };
    let spec = elastic_spec(conn, &owner, &array_id)?;
    tentanas_helper::elastic::validate_elastic_uuid(&operation_id)?;
    anyhow::ensure!(
        job_kind == "elastic_mover" && subject == spec.name && request.len() < 16 * 1024,
        "Niezgodna intencja zadania movera"
    );
    // The stored request is the command the body ran. Its rules come from the
    // array row and cannot be re-derived from the spec, so what is checked here
    // is IDENTITY: a rewritten row must not be able to attach a foreign array
    // or a foreign operation to this job.
    let command: tentanas_helper::HelperCommand = serde_json::from_str(&request)?;
    anyhow::ensure!(
        matches!(&command, tentanas_helper::HelperCommand::ElasticMover {
            array_id: stored_array, owner: stored_owner, operation_id: stored_operation, ..
        } if *stored_array == array_id && *stored_owner == owner && *stored_operation == operation_id),
        "Zmienione żądanie operacji movera"
    );
    Ok(Some((operation_id, spec, candidate)))
}

pub fn record_mover_result(
    pool: &DbPool,
    owner: &ElasticOwner,
    operation_id: &str,
    result: &tentanas_helper::elastic::ElasticMoverResult,
) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let job_id: String = tx.query_row(
        "SELECT o.job_id FROM nas_elastic_operations o
        JOIN nas_elastic_arrays a ON a.array_id=o.array_id
        WHERE o.operation_id=?1 AND o.state='running' AND a.org_id=?2 AND a.addon_id=?3",
        params![operation_id, owner.org_id, owner.addon_id],
        |r| r.get(0),
    )?;
    let (stored_id, spec, candidate) =
        mover_operation(&tx, &job_id)?.ok_or_else(|| anyhow!("Brak intencji movera"))?;
    anyhow::ensure!(
        stored_id == operation_id && candidate.is_none(),
        "Wynik operacji już zapisano"
    );
    super::elastic::validate_mover_result(&spec, operation_id, result)?;
    let json = serde_json::to_string(result)?;
    anyhow::ensure!(json.len() < 64 * 1024, "Za duży wynik movera");
    anyhow::ensure!(tx.execute("UPDATE nas_elastic_operations SET result_json=?2 WHERE operation_id=?1 AND state='running' AND result_json IS NULL",
        params![operation_id,json])? == 1, "Nie zapisano kandydata movera");
    tx.commit()?;
    Ok(())
}

pub fn job(pool: &DbPool, job_id: &str) -> Result<Option<NasJob>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!("SELECT {JOB_COLUMNS} FROM nas_jobs WHERE job_id = ?1"),
            params![job_id],
            job_from_row,
        )
        .optional()?)
}

pub fn list_jobs(pool: &DbPool, limit: u32) -> Result<Vec<NasJob>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {JOB_COLUMNS} FROM nas_jobs ORDER BY started_at DESC LIMIT ?1"
    ))?;
    let rows = stmt
        .query_map(params![i64::from(limit.clamp(1, 500))], job_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `list_jobs` as one organisation may see it (migration 18): its own jobs
/// and the node-wide ones, never another tenant's array work — whose subject
/// is that array's name and whose log and author are that tenant's. The
/// unscoped `list_jobs` stays for the node's own loops.
pub fn list_jobs_for_org(pool: &DbPool, org_id: &str, limit: u32) -> Result<Vec<NasJob>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {JOB_COLUMNS} FROM nas_jobs WHERE {VISIBLE_TO_ORG_SQL}
         ORDER BY started_at DESC LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![org_id, i64::from(limit.clamp(1, 500))], job_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One job, when `org_id` may see it. Another tenant's job is `None` — the
/// same answer as an id that never existed, so a guessed id confirms nothing.
pub fn job_for_org(pool: &DbPool, org_id: &str, job_id: &str) -> Result<Option<NasJob>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!("SELECT {JOB_COLUMNS} FROM nas_jobs WHERE job_id = ?2 AND {VISIBLE_TO_ORG_SQL}"),
            params![org_id, job_id],
            job_from_row,
        )
        .optional()?)
}

/// Stamps an Elastic job with the organisation that owns its array, inside
/// the transaction that wrote the job. Every Elastic job's subject is its
/// array's name (unique per node), and the array row exists by now — for a
/// create or an adoption it was written earlier in this same transaction.
/// An array that cannot be found stamps '' (nobody's): see migration 18.
fn stamp_elastic_job_org(tx: &rusqlite::Transaction<'_>, job_id: &str, kind: &str, subject: &str) -> Result<()> {
    if !kind.starts_with("elastic_") {
        return Ok(());
    }
    tx.execute(
        "UPDATE nas_jobs SET org_id = COALESCE((SELECT org_id FROM nas_elastic_arrays WHERE name = ?2), '')
          WHERE job_id = ?1",
        params![job_id, subject],
    )?;
    Ok(())
}

/// Jobs that were `running` when the process died: marked failed on init so
/// the list never shows a spinner for work nobody is doing.
///
/// KNOWN EXPOSURE — a long self-test job versus this sweep (medium severity,
/// accepted deliberately, not an oversight):
///
/// A SMART self-test job now holds its `running` row for the disk's REAL test
/// duration — 24.5 h on the captured SAS disk, and up to twice that before the
/// stall window gives up — where it used to last about two minutes, because the
/// poll misread a missing progress percentage as completion. Any core restart,
/// upgrade or crash inside that window therefore lands here and stamps a
/// healthy, still-running test `failed` / "interrupted by core restart" while
/// the disk quietly keeps testing.
///
/// It is not a lost result: the real verdict reaches the disk detail view and
/// `score_health` as soon as the periodic `refresh_smart` reads the self-test
/// log again. The operator gets a misleading job row, not a missing answer —
/// which is why this is documented rather than worked around here.
///
/// FOLLOW-UP "short-lived self-test job": the honest fix is that a self-test
/// should not occupy a `running` row at all. The disk owns the test; the app
/// only observes it. Starting the test, persisting `started_at` plus the
/// baseline log, and letting `refresh_smart` surface the verdict would make the
/// job short-lived and restart-immune. That is a redesign of the job's
/// contract, so it is named here and left for its own change.
/// The history outcome of a Sync or a Scrub whose process never reported.
pub const INTERRUPTED_OUTCOME: &str = "interrupted";

pub fn fail_orphaned_jobs(pool: &DbPool) -> Result<usize> {
    let running = super::jobs::running().lock().unwrap_or_else(|p| p.into_inner());
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let candidates = tx.prepare("SELECT job_id FROM nas_jobs WHERE status IN ('queued','running')")?
        .query_map([],|r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut changed = 0;
    // Every candidate is failed unconditionally — including a SMART self-test
    // whose disk is still testing and will finish hours from now. See the
    // exposure and the "short-lived self-test job" follow-up on this function.
    for job_id in candidates.into_iter().filter(|id| !running.contains_key(id)) {
        tx.execute("UPDATE nas_elastic_arrays SET state='needs_attention',
        state_detail='Utracono nadzór core; stan zadania nie dowodzi zakończenia I/O', updated_at=?1
        WHERE array_id IN (SELECT array_id FROM nas_elastic_operations WHERE state='running' AND job_id=?2)",
        params![now(),job_id])?;
        tx.execute("UPDATE nas_elastic_operations SET state='needs_attention',
        error=?3, finished_at=?1
        WHERE state='running' AND job_id=?2", params![now(),job_id,ORPHANED_OPERATION_ERROR])?;
        changed += tx.execute(
        "UPDATE nas_jobs SET status = 'failed', error = 'interrupted by core restart',
                finished_at = ?1
         WHERE status IN ('queued', 'running') AND job_id=?2",
        params![now(),job_id],
        )?;
    }
    tx.commit()?;
    Ok(changed)
}

/// The persisted intention of one array, read from its ORIGIN operation — the
/// create that made it, or the import that adopted it. Migration 14 allows
/// exactly one of the two per array, so this join still returns a single row.
fn elastic_spec(conn: &Connection, owner: &ElasticOwner, array_id: &str) -> Result<ElasticCreateSpec> {
    let (name, filesystem, json): (String, String, String) = conn.query_row(
        "SELECT a.name,a.filesystem,o.request_json FROM nas_elastic_arrays a
         JOIN nas_elastic_operations o ON o.array_id=a.array_id AND o.kind IN ('create','import')
         WHERE a.array_id=?1 AND a.org_id=?2 AND a.addon_id=?3",
        params![array_id,owner.org_id,owner.addon_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    anyhow::ensure!(json.len() < 16 * 1024, "Zapis Elastic przekracza limit");
    let spec: ElasticCreateSpec = serde_json::from_str(&json)?;
    spec.validate()?;
    anyhow::ensure!(spec.owner == *owner && spec.array_id == array_id && spec.name == name
        && spec.filesystem.as_str() == filesystem, "Niezgodna tożsamość zapisanej macierzy");
    let mut expected_aliases = std::collections::BTreeSet::new();
    let mut expected_disks = Vec::new();
    for (role, disks) in [("data", &spec.data), ("parity", &spec.parity)] {
        for (index, disk) in disks.iter().enumerate() {
            let slot = i64::try_from(index + 1)?;
            expected_disks.push((role.to_string(),slot,disk.disk_id.clone(),disk.wwn.clone(),
                disk.serial.clone(),i64::try_from(disk.bytes)?,disk.expected_uuid.clone()));
            for (kind,value) in [("disk_id",Some(disk.disk_id.as_str())),
                ("wwn",disk.wwn.as_deref()),("serial",disk.serial.as_deref())] {
                if let Some(value) = value {
                    expected_aliases.insert((kind.to_string(),value.to_string(),role.to_string(),slot));
                }
            }
        }
    }
    if let Some(disk) = spec.cache.as_ref() {
        let role = "cache".to_string();
        let slot = 1;
        expected_disks.push((role.clone(),slot,disk.disk_id.clone(),disk.wwn.clone(),
            disk.serial.clone(),i64::try_from(disk.bytes)?,disk.expected_uuid.clone()));
        for (kind,value) in [("disk_id",Some(disk.disk_id.as_str())),
            ("wwn",disk.wwn.as_deref()),("serial",disk.serial.as_deref())] {
            if let Some(value) = value {
                expected_aliases.insert((kind.to_string(),value.to_string(),role.clone(),slot));
            }
        }
    }
    expected_disks.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    let actual_disks = conn.prepare("SELECT role,slot,disk_id,wwn,serial,bytes,expected_uuid
        FROM nas_elastic_disks WHERE array_id=?1 ORDER BY role,slot")?
        .query_map(params![array_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,
            r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,
            r.get::<_,i64>(5)?,r.get::<_,String>(6)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let actual_aliases = conn.prepare("SELECT kind,value,role,slot
        FROM nas_elastic_disk_aliases WHERE array_id=?1")?
        .query_map(params![array_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,
            r.get::<_,String>(2)?,r.get::<_,i64>(3)?)))?
        .collect::<rusqlite::Result<std::collections::BTreeSet<_>>>()?;
    anyhow::ensure!(actual_disks == expected_disks && actual_aliases == expected_aliases,
        "Rezerwacje dysków nie odpowiadają intencji Elastic");
    Ok(spec)
}

pub fn elastic_arrays(pool: &DbPool, owner: &ElasticOwner) -> Result<Vec<super::elastic::ElasticArrayRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let headers = conn.prepare("SELECT array_id,state,state_detail,created_at,updated_at
        FROM nas_elastic_arrays WHERE org_id=?1 AND addon_id=?2 ORDER BY name")?
        .query_map(params![owner.org_id,owner.addon_id], |r| Ok((r.get::<_,String>(0)?,
            r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // The parity-errors count is read from a DATE window, never from the tail
    // of the short display history: a run with errors that is still inside the
    // window must not be able to rotate out behind newer clean runs. One day of
    // slack below the advertised window keeps a record whose stored timestamp
    // trails the helper's own from being dropped before the producer judges it.
    let parity_since = (chrono::Utc::now()
        - chrono::Duration::days(i64::from(super::elastic::PARITY_ERRORS_WINDOW_DAYS) + 1))
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    headers.into_iter().map(|(array_id,state,state_detail,created_at,updated_at)| {
        let spec = elastic_spec(&conn, owner, &array_id)?;
        // The folders are DISCOVERED, not stored: the union is published on
        // the host, so its real top level is one unprivileged `read_dir` away,
        // and the stored policies are joined onto it. Doing it here and not in
        // `elastic::list` is deliberate — every consumer of a row gets the
        // same folders, the mover that `spawn_mover` derives its rules from
        // included, and there is one place that could get it wrong.
        let union = tentanas_helper::elastic::union_path(&spec.name);
        let (folders, folders_known) = super::elastic::folders_of(
            std::path::Path::new(&union),
            &elastic_folder_policies_on(&conn, &array_id)?,
            &folder_shares_on(&conn, &union)?,
        );
        Ok(super::elastic::ElasticArrayRow {
            folders,
            folders_known,
            snapraid_history: elastic_runs(&conn, &spec, None, false, 20, None)?,
            parity_window_runs: elastic_runs(&conn, &spec, None, false,
                super::elastic::PARITY_ERRORS_MAX_ROWS, Some(&parity_since))?,
            last_sync_run: elastic_runs(&conn, &spec, Some("sync"), true, 1, None)?.into_iter().next(),
            last_scrub_run: elastic_runs(&conn, &spec, Some("scrub"), false, 1, None)?.into_iter().next(),
            name: spec.name.clone(), enabled: true, filesystem: spec.filesystem.as_str().to_string(),
            create_policy: "mfs".to_string(),
            branches: spec.data.iter().enumerate().map(|(i,d)| super::elastic::BranchRow {
                disk_id: d.disk_id.clone(), name: format!("d{}",i+1),
                device: format!("/dev/disk/by-uuid/{}",d.expected_uuid), role: "data".to_string(),
            }).chain(spec.cache.iter().map(|d| super::elastic::BranchRow {
                disk_id: d.disk_id.clone(), name: "c1".to_string(),
                device: format!("/dev/disk/by-uuid/{}",d.expected_uuid), role: "cache".to_string(),
            })).collect(),
            parity: spec.parity.iter().enumerate().map(|(i,d)| super::elastic::ParityRow {
                disk_id: d.disk_id.clone(), name: format!("parity{}",i+1),
                device: format!("/dev/disk/by-uuid/{}",d.expected_uuid), index: (i+1) as u8,
            }).collect(),
            mover: elastic_mover_config(&conn, &array_id)?,
            snapraid: elastic_snapraid_config(&conn, &array_id)?,
            mover_history: mover_runs(&conn, &spec, super::elastic::MOVER_HISTORY_ROWS)?,
            // Survives a later operation returning the array to 'active' —
            // `finish_elastic_operation` only ever touches its OWN row, so a
            // Restore after a failed sync leaves this standing. It is the
            // reason the mover is refused, so the wire has to carry it.
            unresolved_operation: conn.query_row(
                &format!("SELECT {UNRESOLVED_ELASTIC_OPERATION}"),
                params![array_id], |r| r.get(0))?,
            mover_settles_unresolved: mover_admission(&conn, &array_id, &state)?.1,
            // What the button on the screen may offer: a Sync or a Scrub is
            // admitted on the array a failed parity run left behind, and the UI
            // must not disable the one action that resolves it.
            parity_run_available: parity_admission(&conn, &array_id, &state)?,
            mover_failed_runs: mover_failed_runs(&conn, &array_id)?,
            pending_add: pinned_add(&conn, &array_id)?,
            create_spec: Some(spec), state, state_detail, created_at, updated_at,
            ..Default::default()
        })
    }).collect()
}

/// Reads an array's sync, scrub and repair history.
///
/// `since` bounds the STORED `finished_at` from below, RFC3339 in UTC like
/// `now()` writes it, so a caller can ask for a WINDOW instead of a row count.
///
/// A windowed query also drops rows that carry NO finish time. They hold no
/// measurement the caller could use — an unfinished run is not evidence either
/// way — and keeping them would let a burst of stuck operations fill the row
/// cap with rows that say nothing, wedging a card to `unknown` with no visible
/// cause. Callers that ask by count (`since` = None) still see them, because
/// the display history is where an unfinished run belongs.
fn elastic_runs(
    conn: &Connection,
    spec: &ElasticCreateSpec,
    kind: Option<&str>,
    succeeded: bool,
    limit: u32,
    since: Option<&str>,
) -> Result<Vec<tentaflow_protocol::tentanas::NasSnapraidRun>> {
    use tentanas_helper::elastic::{
        ElasticSnapraidKind, ElasticSnapraidOutcome, ElasticSnapraidResult,
    };
    let mut statement = conn.prepare(
        "SELECT operation_id,job_id,kind,state,created_at,finished_at,error,result_json
        FROM nas_elastic_operations WHERE array_id=?1 AND kind IN ('sync','scrub','fix')
        AND (?2 IS NULL OR kind=?2) AND (?3=0 OR state='succeeded')
        AND (?2 IS NULL OR ?3!=0 OR state NOT IN ('running','failed'))
        AND (?5 IS NULL OR finished_at >= ?5)
        ORDER BY created_at DESC,operation_id DESC LIMIT ?4",
    )?;
    let rows = statement.query_map(
        params![
            spec.array_id,
            kind,
            succeeded,
            if kind.is_some() { -1 } else { i64::from(limit) },
            since
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        },
    )?;
    let terminal_only = kind.is_some();
    let mut history = Vec::new();
    for row in rows {
        let (operation_id, job_id, kind, state, started_at, finished_at, detail, json) = row?;
        let stored_finished_at = finished_at.clone();
        let mut terminal_run = false;
        let mut value = tentaflow_protocol::tentanas::NasSnapraidRun {
            job_id: Some(job_id),
            operation_id: Some(operation_id.clone()),
            kind: kind.clone(),
            started_at,
            finished_at,
            outcome: if state == "succeeded" {
                "ok"
            } else {
                state.as_str()
            }
            .into(),
            detail,
            ..Default::default()
        };
        // A row `fail_orphaned_jobs` closed is interrupted whatever its body
        // had recorded: the job never finished, and the SQL admission treats it
        // as resolved on exactly that error, so the history must not show it as
        // a fault with counters (the LOW inconsistency of the third review).
        let interrupted = state == "needs_attention"
            && kind != "fix"
            && (json.is_none() || value.detail == ORPHANED_OPERATION_ERROR);
        if state != "running" {
            let decoded = json
                .as_deref()
                .ok_or_else(|| anyhow!("Brak potwierdzonego wyniku"))
                .and_then(|json| {
                    let mut result: ElasticSnapraidResult = serde_json::from_str(json)?;
                    let pinned = pinned_add(conn, &spec.array_id)?;
                    super::elastic::strip_unpinned_slot(spec, pinned.as_ref(), &mut result.state);
                    // The repair's disk comes from the RESULT and is then
                    // held to the array by `validate_snapraid_result`: the
                    // operations table records the action, not the disk, and a
                    // history row may not invent one the array never had.
                    let expected = match kind.as_str() {
                        "sync" => ElasticSnapraidKind::Sync,
                        "scrub" => ElasticSnapraidKind::Scrub,
                        _ => result.run.kind.clone(),
                    };
                    // A record is judged against the array it described:
                    // older than an add that succeeded since, or counting an
                    // add that is still pinned (K5).
                    let recorded = super::elastic::recorded_spec(spec, pinned.as_ref(), &result.state)?;
                    super::elastic::validate_snapraid_result(
                        &recorded,
                        &operation_id,
                        &expected,
                        &result,
                    )?;
                    Ok(result)
                });
            match decoded {
                Ok(result)
                    if !interrupted
                        && !(state == "needs_attention"
                            && result.run.outcome == ElasticSnapraidOutcome::Succeeded) =>
                {
                    let run = result.run;
                    // A partial Sync is listed, but it is never the array's
                    // last COMPLETE Sync: parity was not made current by it.
                    terminal_run = matches!(
                        run.outcome,
                        ElasticSnapraidOutcome::Succeeded | ElasticSnapraidOutcome::Failed
                    );
                    value.started_at = run.started_at;
                    value.finished_at = run.finished_at;
                    value.outcome = match run.outcome {
                        ElasticSnapraidOutcome::Running => {
                            return Err(anyhow!("Nieterminalna historia SnapRAID"));
                        }
                        outcome => super::elastic::snapraid_outcome_to_protocol(outcome),
                    };
                    value.detail = run.detail.unwrap_or(value.detail);
                    value.exit_code = run.exit_code;
                    value.total_blocks = run.total_blocks;
                    value.checked_blocks = run.checked_blocks;
                    value.accessed_mb = run.accessed_mb;
                    value.errors_file = run.errors_file;
                    value.errors_io = run.errors_io;
                    value.errors_data = run.errors_data;
                    value.errors = run
                        .errors_file
                        .zip(run.errors_io)
                        .zip(run.errors_data)
                        .and_then(|((file, io), data)| file.checked_add(io)?.checked_add(data));
                }
                // Refused before it started because another Elastic command
                // held the lock: nothing ran, so no result was ever recorded.
                Err(_) if state == "failed" && json.is_none() && nothing_ran(Some(&value.detail)) => {
                    value.outcome = super::elastic::snapraid_outcome_to_protocol(ElasticSnapraidOutcome::Refused);
                }
                // A Sync or a Scrub whose process never reported, or whose job
                // core lost: interrupted, which is neither a failure nor
                // evidence for a repair.
                _ if interrupted => {
                    value.outcome = INTERRUPTED_OUTCOME.into();
                    value.finished_at = None;
                }
                _ => {
                    anyhow::ensure!(
                        state == "needs_attention",
                        "Niespójna utrwalona historia SnapRAID"
                    );
                    value.finished_at = None;
                }
            }
        }
        // MIN-3: a WINDOW must be judged on the same timestamp it was filtered
        // on. The SQL bound above reads the stored `finished_at`, written by
        // THIS node's clock; the decoding above then overwrites the value with
        // the finish time the HELPER reported, from another machine's clock.
        // Comparing one against the other is a skew hole: a helper running far
        // enough ahead puts a run inside the window that the query had already
        // dropped, and the rows that remain would answer a confident zero. So a
        // windowed row carries the stored time — one clock, one comparison, and
        // no slack to buy. The display history is untouched: it shows the run's
        // own reported finish, which is what an operator asked to see.
        if since.is_some() {
            value.finished_at = stored_finished_at.clone();
        }
        if terminal_only && !terminal_run {
            continue;
        }
        history.push(value);
        if history.len() >= limit as usize {
            break;
        }
    }
    Ok(history)
}

/// One array's RECORDED mover runs, newest first — n11's history strip.
///
/// Only rows whose stored result decodes and validates are listed. A run with
/// no trustworthy measurement has no byte count to show, and this strip exists
/// to say how much each run moved; emitting it with zeroed counters would print
/// exactly the fabricated zero this feature is shaped against. Such a run is
/// still visible as the array's state and as `last_run`, so nothing is hidden —
/// only the number nobody measured. A row that fails to decode while NOT closed
/// as needs_attention is a contradiction and fails the read, the same way
/// `elastic_runs` treats an inconsistent SnapRAID row.
fn mover_runs(
    conn: &Connection,
    spec: &ElasticCreateSpec,
    limit: u32,
) -> Result<Vec<tentaflow_protocol::tentanas::NasMoverRun>> {
    let mut statement = conn.prepare(
        "SELECT operation_id,state,result_json FROM nas_elastic_operations
        WHERE array_id=?1 AND kind='mover' AND result_json IS NOT NULL
        ORDER BY created_at DESC,operation_id DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![spec.array_id, i64::from(limit)], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut history = Vec::new();
    for row in rows {
        let (operation_id, state, json) = row?;
        anyhow::ensure!(json.len() < 64 * 1024, "Za duży wynik movera");
        let decoded = serde_json::from_str::<tentanas_helper::elastic::ElasticMoverResult>(&json)
            .map_err(anyhow::Error::from)
            .and_then(|result| {
                super::elastic::validate_mover_result(spec, &operation_id, &result)?;
                Ok(result)
            });
        match decoded {
            Ok(result) => history.push(super::elastic::mover_to_protocol(&result.run)),
            // A row nothing can read is dropped from the strip rather than
            // fabricated into a zero — but it must not take the whole array
            // read down with it either. Failing here made ONE unreadable row
            // render the entire detail screen unloadable, which is strictly
            // worse than showing a shorter history: the two honest options are
            // "skip it" and "brick the view", and only the first keeps the rest
            // of the array's measured state visible. The drop is logged so an
            // operator can see it even though the strip cannot say it.
            Err(error) => tracing::warn!(
                "tentanas mover history: pominięto nieczytelną operację {operation_id} \
                 (stan {state}): {error}"
            ),
        }
    }
    Ok(history)
}

pub fn elastic_array(pool: &DbPool, owner: &ElasticOwner, name: &str) -> Result<Option<super::elastic::ElasticArrayRow>> {
    Ok(elastic_arrays(pool, owner)?.into_iter().find(|a| a.name == name))
}

/// The names of the Elastic Arrays one organisation owns on this node — the
/// set `disks::hide_other_org_array` keeps names for. By organisation, not by
/// instance: TentaNas is one instance per node, so the org is the tenant.
pub fn elastic_array_names_of_org(pool: &DbPool, org_id: &str) -> Result<std::collections::BTreeSet<String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached("SELECT name FROM nas_elastic_arrays WHERE org_id = ?1")?;
    let names = stmt
        .query_map(params![org_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<std::collections::BTreeSet<_>>>()?;
    Ok(names)
}

pub fn elastic_claims(pool: &DbPool) -> Result<Vec<ElasticClaim>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let rows = conn.prepare("SELECT array_id,org_id,addon_id FROM nas_elastic_arrays ORDER BY array_id")?
        .query_map([], |r| Ok((r.get::<_,String>(0)?,ElasticOwner {org_id:r.get(1)?,addon_id:r.get(2)?})))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut claims = Vec::new();
    for (array_id,owner) in rows {
        let spec=elastic_spec(&conn,&owner,&array_id)?;
        claims.extend(spec.data.into_iter().chain(spec.cache).chain(spec.parity).map(|disk|
            ElasticClaim {disk_id:disk.disk_id,wwn:disk.wwn,serial:disk.serial}));
    }
    Ok(claims)
}

/// Every member of a spec with the role and slot its rows take: the data disks
/// in order from slot 1, the cache disk at `cache`/1, the parity disks in
/// order. The create and the import write the same three tables from this, so
/// the two paths cannot disagree about which slot a disk lands in.
fn elastic_disk_slots(spec: &ElasticCreateSpec) -> Result<Vec<(&'static str, i64, &ElasticDiskSpec)>> {
    let mut slots = Vec::new();
    for (role, disks) in [("data", &spec.data), ("parity", &spec.parity)] {
        for (index, disk) in disks.iter().enumerate() {
            slots.push((role, i64::try_from(index + 1)?, disk));
        }
    }
    if let Some(disk) = spec.cache.as_ref() {
        slots.push(("cache", 1, disk));
    }
    Ok(slots)
}

/// Writes the member rows and their aliases of one array. The UNIQUE columns
/// (`disk_id`, `expected_uuid`) and the alias primary key are the reservation
/// itself: two arrays can never hold the same disk, whichever path wrote them.
fn insert_elastic_disks(tx: &Connection, spec: &ElasticCreateSpec) -> Result<()> {
    for (role, slot, disk) in elastic_disk_slots(spec)? {
        tx.execute("INSERT INTO nas_elastic_disks
            (array_id,role,slot,disk_id,wwn,serial,bytes,expected_uuid)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![spec.array_id,role,slot,disk.disk_id,disk.wwn,disk.serial,
                i64::try_from(disk.bytes)?,disk.expected_uuid])?;
        for (kind, value) in [("disk_id", Some(disk.disk_id.as_str())),
            ("wwn",disk.wwn.as_deref()),("serial",disk.serial.as_deref())] {
            if let Some(value) = value {
                tx.execute("INSERT INTO nas_elastic_disk_aliases
                    (kind,value,array_id,role,slot) VALUES (?1,?2,?3,?4,?5)",
                    params![kind,value,spec.array_id,role,slot])?;
            }
        }
    }
    Ok(())
}

/// Checks that one disk may join an array, under the SAME rules the member
/// rows enforce once it has.
///
/// It exists because the row cannot be written yet: `elastic_spec` cross-checks
/// `nas_elastic_disks` against the array's persisted intention, and until the
/// add closes that intention is still the array WITHOUT this disk — so writing
/// the row first would make every read of the array fail for the length of the
/// operation. The reservation is therefore the running operation row, and these
/// are the checks its `UNIQUE` columns would have made.
fn reserve_added_disk(tx: &Connection, array_id: &str, disk: &ElasticDiskSpec) -> Result<bool> {
    let claimed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM nas_elastic_disks WHERE disk_id=?1 OR expected_uuid=?2)",
        params![disk.disk_id, disk.expected_uuid],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        !claimed,
        "Dysk {} należy już do macierzy tej instancji",
        disk.disk_id
    );
    for value in [
        Some(disk.disk_id.as_str()),
        disk.wwn.as_deref(),
        disk.serial.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let reserved: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM nas_elastic_disk_aliases WHERE value=?1)",
            params![value],
            |r| r.get(0),
        )?;
        anyhow::ensure!(
            !reserved,
            "Identyfikator {value} jest już zarezerwowany przez macierz tej instancji"
        );
    }
    // Another add of this array that stopped part-way holds the slot and —
    // more importantly — the filesystem UUID its mkfs was given. A retry has
    // to reuse that identity, so a DIFFERENT disk arriving for the same slot
    // is refused here rather than formatting a second device, while the SAME
    // disk is admitted and reported as the retry it is. An add the helper
    // REFUSED closed `failed` and holds nothing (K4): it never touched the
    // helper's journal, so there is no slot to reuse.
    let Some((state, held)) = latest_add_disk(tx, array_id)? else {
        return Ok(false);
    };
    if state == "succeeded" {
        return Ok(false);
    }
    // A RUNNING add is not a resumable one. Treating it as one flipped its row
    // out of `running` — which is what the partial unique index refuses a
    // second operation by — and a second helper invocation then raced the first
    // over the same slot: the disk ends up joined to the live union with no
    // member row, because the loser's `finish_elastic_add_disk` finds no
    // running operation of its own. Wait for the verdict instead.
    anyhow::ensure!(
        state != "running",
        "Dodanie dysku {} do tej macierzy jest w toku; poczekaj na jego wynik",
        held.disk_id
    );
    anyhow::ensure!(
        held == *disk,
        "Poprzednie dodanie dysku {} nie zostało zakończone; powtórz je tym samym dyskiem",
        held.disk_id
    );
    Ok(true)
}

/// This array's LATEST add that the helper may hold, as `(state, disk)`.
///
/// The question is about the latest attempt and not about "any attempt that
/// failed": an array that was grown after one failed attempt carries both rows
/// for ever, and reading the failed one would hand a caller the identity of an
/// add that a later one already completed — pinning the array to a disk it
/// already holds. So the newest row is read, and the STATE is returned with it
/// because it decides three different things: a succeeded add holds nothing,
/// a running one may not be touched, and only a stopped one
/// (`needs_attention`, the orphan of a core restart included) is resumable.
///
/// A `failed` row is SKIPPED, never read as the newest (K4). It is a refusal
/// — the helper changed nothing — or an attempt a later one superseded, so it
/// holds no slot: reading it would let a refused retry of a stopped add
/// unpin that add, and the disk would sit half-joined with no way through
/// the product to finish it. `(created_at, operation_id)` is the ordering for
/// the reason `UNRESOLVED_ELASTIC_OPERATION` uses it: `created_at` has second
/// resolution and an operation id is a uuid v7.
fn latest_add_disk(
    conn: &Connection,
    array_id: &str,
) -> Result<Option<(String, ElasticDiskSpec)>> {
    let latest: Option<(String, String)> = conn
        .query_row(
            "SELECT state,request_json FROM nas_elastic_operations
             WHERE array_id=?1 AND kind='add_disk' AND state<>'failed'
             ORDER BY created_at DESC, operation_id DESC LIMIT 1",
            params![array_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((state, json)) = latest else {
        return Ok(None);
    };
    match serde_json::from_str::<tentanas_helper::HelperCommand>(&json)? {
        tentanas_helper::HelperCommand::ElasticAddDisk { disk, .. } => Ok(Some((state, disk))),
        _ => Err(anyhow!("Niezgodna intencja dodania dysku")),
    }
}

/// The newest replacement operation of this array, as (state, branch, disk).
fn latest_replace_disk(
    conn: &Connection,
    array_id: &str,
) -> Result<Option<(String, String, ElasticDiskSpec)>> {
    let latest: Option<(String, String)> = conn
        .query_row(
            "SELECT state,request_json FROM nas_elastic_operations
             WHERE array_id=?1 AND kind='replace_disk'
             ORDER BY created_at DESC, operation_id DESC LIMIT 1",
            params![array_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((state, json)) = latest else {
        return Ok(None);
    };
    match serde_json::from_str::<tentanas_helper::HelperCommand>(&json)? {
        tentanas_helper::HelperCommand::ElasticReplaceDisk { branch, disk, .. } => {
            Ok(Some((state, branch, disk)))
        }
        _ => Err(anyhow!("Niezgodna intencja wymiany dysku")),
    }
}

/// The slot and the identity a repeat of an unfinished replacement must reuse.
///
/// The filesystem UUID is the reason, exactly as for an add: it is what the
/// mkfs stamped on the replacement and what `filesystem_matches` checks the
/// branch by, so a repeat that minted a new one would format a disk the
/// journal already calls a member of the array.
pub fn unfinished_elastic_replace_disk(
    pool: &DbPool,
    owner: &ElasticOwner,
    array_id: &str,
) -> Result<Option<(String, ElasticDiskSpec)>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mine: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM nas_elastic_arrays
         WHERE array_id=?1 AND org_id=?2 AND addon_id=?3)",
        params![array_id, owner.org_id, owner.addon_id],
        |r| r.get(0),
    )?;
    if !mine {
        return Ok(None);
    }
    // A RUNNING replacement is deliberately not offered: the caller asks in
    // order to decide whether it may repeat the operation, and one still in
    // flight may not be repeated.
    Ok(latest_replace_disk(&conn, array_id)?
        .filter(|(state, _, _)| !matches!(state.as_str(), "succeeded" | "running"))
        .map(|(_, branch, disk)| (branch, disk)))
}

/// Closes a successful replacement: the member row, its aliases and the
/// array's persisted intention all describe the NEW disk in one transaction,
/// for the reason `finish_elastic_add_disk` gives — `elastic_spec` refuses an
/// intention its member rows do not match, so a commit that landed one
/// without the other would make every later read of this array fail.
pub fn finish_elastic_replace_disk(
    pool: &DbPool,
    owner: &ElasticOwner,
    operation_id: &str,
    branch: &str,
    disk: &ElasticDiskSpec,
    observed: &ElasticResult,
) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let array_id: String = tx.query_row(
        "SELECT o.array_id FROM nas_elastic_operations o
         JOIN nas_elastic_arrays a ON a.array_id=o.array_id
         WHERE o.operation_id=?1 AND o.kind='replace_disk' AND o.state='running'
         AND a.org_id=?2 AND a.addon_id=?3",
        params![operation_id, owner.org_id, owner.addon_id],
        |r| r.get(0),
    )?;
    let before = elastic_spec(&tx, owner, &array_id)?;
    let after = super::elastic::spec_with_replaced_disk(&before, branch, disk)?;
    super::elastic::validate_result(&after, observed)?;
    let slot = i64::try_from(
        after
            .data
            .iter()
            .position(|member| member.disk_id == disk.disk_id)
            .ok_or_else(|| anyhow!("Wymieniony dysk nie jest w zapisanej intencji"))?
            + 1,
    )?;
    tx.execute(
        "UPDATE nas_elastic_disks SET disk_id=?3,wwn=?4,serial=?5,bytes=?6,expected_uuid=?7
         WHERE array_id=?1 AND role='data' AND slot=?2",
        params![
            array_id,
            slot,
            disk.disk_id,
            disk.wwn,
            disk.serial,
            i64::try_from(disk.bytes)?,
            disk.expected_uuid
        ],
    )?;
    tx.execute(
        "DELETE FROM nas_elastic_disk_aliases WHERE array_id=?1 AND role='data' AND slot=?2",
        params![array_id, slot],
    )?;
    for (kind, value) in [
        ("disk_id", Some(disk.disk_id.as_str())),
        ("wwn", disk.wwn.as_deref()),
        ("serial", disk.serial.as_deref()),
    ] {
        if let Some(value) = value {
            tx.execute(
                "INSERT INTO nas_elastic_disk_aliases (kind,value,array_id,role,slot)
                 VALUES (?1,?2,?3,'data',?4)",
                params![kind, value, array_id, slot],
            )?;
        }
    }
    let json = serde_json::to_string(&after)?;
    anyhow::ensure!(json.len() < 16 * 1024, "Intencja Elastic przekracza limit");
    anyhow::ensure!(
        tx.execute(
            "UPDATE nas_elastic_operations SET request_json=?2
             WHERE array_id=?1 AND kind IN ('create','import')",
            params![array_id, json]
        )? == 1,
        "Brak jednej operacji źródłowej macierzy"
    );
    // The read-back is the proof the writes agree: it is the very function
    // every later read of this array goes through.
    anyhow::ensure!(
        elastic_spec(&tx, owner, &array_id)? == after,
        "Zapis wymiany dysku nie odtworzył intencji macierzy"
    );
    let at = now();
    tx.execute(
        "UPDATE nas_elastic_operations SET state='succeeded',result_json=?2,error='',finished_at=?3
         WHERE operation_id=?1",
        params![operation_id, serde_json::to_string(observed)?, at],
    )?;
    tx.execute(
        "UPDATE nas_elastic_arrays SET state='active',state_detail='',updated_at=?2
         WHERE array_id=?1",
        params![array_id, at],
    )?;
    tx.commit()?;
    Ok(())
}

/// The disk spec a retry of an unfinished add must reuse, if there is one.
///
/// The filesystem UUID is the reason: it is what the mkfs stamped on the disk
/// and what `filesystem_matches` checks the branch by, so a retry that minted a
/// new one would either format the disk a second time or refuse to mount it.
pub fn unfinished_elastic_add_disk(
    pool: &DbPool,
    owner: &ElasticOwner,
    array_id: &str,
) -> Result<Option<ElasticDiskSpec>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mine: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM nas_elastic_arrays
         WHERE array_id=?1 AND org_id=?2 AND addon_id=?3)",
        params![array_id, owner.org_id, owner.addon_id],
        |r| r.get(0),
    )?;
    if !mine {
        return Ok(None);
    }
    // A running add is deliberately NOT offered: the caller asks this in order
    // to decide whether it may repeat the operation, and an add still in
    // flight may not be repeated. `reserve_added_disk` refuses it by name in
    // the transaction, so the answer here is simply "nothing to resume".
    pinned_add(&conn, array_id)
}

/// The add a helper answer about this array may count (K5/K6): the latest
/// add that stopped part-way and that nothing has finished, undone or
/// superseded since. Refusals never pin (`latest_add_disk`).
fn pinned_add(conn: &Connection, array_id: &str) -> Result<Option<ElasticDiskSpec>> {
    Ok(latest_add_disk(conn, array_id)?
        .filter(|(state, _)| state == "needs_attention")
        .map(|(_, disk)| disk))
}

/// Closes an operation the helper REFUSED before it changed anything (I5):
/// the row is `failed` with the refusal as its error, and the ARRAY is not
/// touched — no state, no detail. A `failed` add pins nothing
/// (`latest_add_disk`), so the array admits another disk at once.
pub fn refuse_elastic_operation(
    pool: &DbPool,
    owner: &ElasticOwner,
    operation_id: &str,
    error: &str,
) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    anyhow::ensure!(
        tx.execute(
            "UPDATE nas_elastic_operations SET state='failed',error=?2,finished_at=?3
             WHERE operation_id=?1 AND state='running'
               AND array_id IN (SELECT array_id FROM nas_elastic_arrays WHERE org_id=?4 AND addon_id=?5)",
            params![operation_id, error, now(), owner.org_id, owner.addon_id],
        )? == 1,
        "Utracono running operację Elastic"
    );
    tx.commit()?;
    Ok(())
}

/// Settles an add the helper no longer holds although this database still
/// pins it (M4 of the release review): the helper answered about the array
/// WITHOUT the disk, with no add recorded or reported (`elastic::
/// add_undone_by_helper`). A core restart during a successful undo leaves
/// exactly that — the undo row orphaned, the add row still pinned — and
/// without this the only offer left would be the resume, which formats and
/// adds the very disk the admin took out. The pinned rows close as history,
/// and the array is `active` again when the helper says Ready and nothing
/// else stands unresolved.
pub fn settle_undone_add(
    pool: &DbPool,
    owner: &ElasticOwner,
    array_id: &str,
    observed: &ElasticResult,
) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let spec = elastic_spec(&tx, owner, array_id)?;
    super::elastic::validate_observation(&spec, observed)?;
    anyhow::ensure!(
        observed.pending_add.is_none()
            && !matches!(observed.attention, Some(tentanas_helper::elastic::ElasticAttention::AddDisk { .. })),
        "Helper nadal zgłasza niedokończone dodanie"
    );
    if pinned_add(&tx, array_id)?.is_none() {
        return Ok(());
    }
    let at = now();
    tx.execute(
        "UPDATE nas_elastic_operations SET state='failed',error=?2,finished_at=COALESCE(finished_at,?3)
         WHERE array_id=?1 AND kind='add_disk' AND state='needs_attention'",
        params![array_id, "wycofane: helper nie przechowuje już tego dodania", at],
    )?;
    let unresolved: bool = tx.query_row(
        &format!("SELECT {UNRESOLVED_ELASTIC_OPERATION}"),
        params![array_id],
        |r| r.get(0),
    )?;
    if observed.stage == tentanas_helper::elastic::ElasticStage::Ready && !unresolved {
        tx.execute(
            "UPDATE nas_elastic_arrays SET state='active',state_detail='',updated_at=?2 WHERE array_id=?1",
            params![array_id, at],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Closes a successful UNDO of an unfinished add (K7). The pinned add rows
/// are closed `failed` — the add did stop, and it no longer holds the array
/// — so nothing pins the slot any more, and the array is `active` again when
/// the helper reports it Ready and nothing else stands unresolved.
pub fn finish_elastic_add_disk_abort(
    pool: &DbPool,
    owner: &ElasticOwner,
    operation_id: &str,
    observed: &ElasticResult,
) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let array_id: String = tx.query_row(
        "SELECT o.array_id FROM nas_elastic_operations o
         JOIN nas_elastic_arrays a ON a.array_id=o.array_id
         WHERE o.operation_id=?1 AND o.kind='add_disk_abort' AND o.state='running'
         AND a.org_id=?2 AND a.addon_id=?3",
        params![operation_id, owner.org_id, owner.addon_id],
        |r| r.get(0),
    )?;
    let spec = elastic_spec(&tx, owner, &array_id)?;
    super::elastic::validate_observation(&spec, observed)?;
    anyhow::ensure!(observed.pending_add.is_none(), "Helper nadal zgłasza niedokończone dodanie");
    let at = now();
    tx.execute(
        "UPDATE nas_elastic_operations SET state='failed',error=?2,finished_at=COALESCE(finished_at,?3)
         WHERE array_id=?1 AND kind='add_disk' AND state='needs_attention'",
        params![array_id, format!("wycofane operacją {operation_id}"), at],
    )?;
    tx.execute(
        "UPDATE nas_elastic_operations SET state='succeeded',result_json=?2,error='',finished_at=?3
         WHERE operation_id=?1",
        params![operation_id, serde_json::to_string(observed)?, at],
    )?;
    let unresolved: bool = tx.query_row(
        &format!("SELECT {UNRESOLVED_ELASTIC_OPERATION}"),
        params![array_id],
        |r| r.get(0),
    )?;
    let ready = observed.stage == tentanas_helper::elastic::ElasticStage::Ready && !unresolved;
    tx.execute(
        "UPDATE nas_elastic_arrays SET state=?2,state_detail=?3,updated_at=?4 WHERE array_id=?1",
        params![
            array_id,
            if ready { "active" } else { "needs_attention" },
            if ready { String::new() } else { observed.detail.clone().unwrap_or_default() },
            at
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// Closes a successful add by making the array's persisted intention and its
/// member rows BOTH describe the array with the new disk — in one transaction.
///
/// They cannot be written apart. `elastic_spec` reads the intention from the
/// origin operation's `request_json` and then refuses it unless
/// `nas_elastic_disks` matches it exactly, so a commit that landed one without
/// the other would make every read of the array fail.
pub fn finish_elastic_add_disk(
    pool: &DbPool,
    owner: &ElasticOwner,
    operation_id: &str,
    disk: &ElasticDiskSpec,
    observed: &ElasticResult,
) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let array_id: String = tx.query_row(
        "SELECT o.array_id FROM nas_elastic_operations o
         JOIN nas_elastic_arrays a ON a.array_id=o.array_id
         WHERE o.operation_id=?1 AND o.kind='add_disk' AND o.state='running'
         AND a.org_id=?2 AND a.addon_id=?3",
        params![operation_id, owner.org_id, owner.addon_id],
        |r| r.get(0),
    )?;
    let before = elastic_spec(&tx, owner, &array_id)?;
    let after = super::elastic::spec_with_added_disk(&before, disk)?;
    super::elastic::validate_result(&after, observed)?;
    let slot = i64::try_from(after.data.len())?;
    tx.execute(
        "INSERT INTO nas_elastic_disks
            (array_id,role,slot,disk_id,wwn,serial,bytes,expected_uuid)
         VALUES (?1,'data',?2,?3,?4,?5,?6,?7)",
        params![
            array_id,
            slot,
            disk.disk_id,
            disk.wwn,
            disk.serial,
            i64::try_from(disk.bytes)?,
            disk.expected_uuid
        ],
    )?;
    for (kind, value) in [
        ("disk_id", Some(disk.disk_id.as_str())),
        ("wwn", disk.wwn.as_deref()),
        ("serial", disk.serial.as_deref()),
    ] {
        if let Some(value) = value {
            tx.execute(
                "INSERT INTO nas_elastic_disk_aliases (kind,value,array_id,role,slot)
                 VALUES (?1,?2,?3,'data',?4)",
                params![kind, value, array_id, slot],
            )?;
        }
    }
    let json = serde_json::to_string(&after)?;
    anyhow::ensure!(json.len() < 16 * 1024, "Intencja Elastic przekracza limit");
    anyhow::ensure!(
        tx.execute(
            "UPDATE nas_elastic_operations SET request_json=?2
             WHERE array_id=?1 AND kind IN ('create','import')",
            params![array_id, json]
        )? == 1,
        "Brak jednej operacji źródłowej macierzy"
    );
    // The read-back is the proof the two writes agree: it is the very function
    // every later read of this array goes through.
    anyhow::ensure!(
        elastic_spec(&tx, owner, &array_id)? == after,
        "Zapis dysku nie odtworzył intencji macierzy"
    );
    let at = now();
    // The attempts this add resumed stop holding the array only NOW, with its
    // success (K4). Closing them when the resume was merely admitted let a
    // resume the helper then refused unpin the add it was resuming.
    tx.execute(
        "UPDATE nas_elastic_operations SET state='failed',error=?3,
            finished_at=COALESCE(finished_at,?4)
         WHERE array_id=?1 AND kind='add_disk' AND state='needs_attention' AND operation_id<>?2",
        params![array_id, operation_id, format!("dokończone operacją {operation_id}"), at],
    )?;
    tx.execute(
        "UPDATE nas_elastic_operations SET state='succeeded',result_json=?2,error='',finished_at=?3
         WHERE operation_id=?1",
        params![operation_id, serde_json::to_string(observed)?, at],
    )?;
    tx.execute(
        "UPDATE nas_elastic_arrays SET state='active',state_detail='',updated_at=?2
         WHERE array_id=?1",
        params![array_id, at],
    )?;
    tx.commit()?;
    Ok(())
}

/// Forgets one array: every reservation, schedule, setting and operation row,
/// then the array itself.
///
/// Child-before-parent, in one immediate transaction, because every foreign key
/// into `nas_elastic_arrays` is `ON DELETE RESTRICT` — that is what makes an
/// array a RESERVATION rather than a note, and it is also why `db::block_elastic_teardown`
/// refuses an uninstall while any array row stands. This is the only function
/// that removes one, and it removes nothing else: no filesystem, no journal, no
/// parity file. The array import takes the array back from those.
///
/// `owner` scopes it because the disks of another addon's array are that
/// addon's reservation; without the scope one instance's destroy could release
/// another's storage.
pub fn delete_elastic_array(pool: &DbPool, owner: &ElasticOwner, array_id: &str) -> Result<bool> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mine: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM nas_elastic_arrays
         WHERE array_id=?1 AND org_id=?2 AND addon_id=?3)",
        params![array_id, owner.org_id, owner.addon_id],
        |r| r.get(0),
    )?;
    if !mine {
        return Ok(false);
    }
    // Aliases reference the member rows, which reference the array — so the
    // order here is the reverse of the order `insert_elastic_disks` wrote them
    // in, and any other order is refused by the keys rather than silently
    // leaving an orphan.
    for statement in [
        "DELETE FROM nas_elastic_disk_aliases WHERE array_id=?1",
        "DELETE FROM nas_elastic_disks WHERE array_id=?1",
        "DELETE FROM nas_elastic_schedules WHERE array_id=?1",
        "DELETE FROM nas_elastic_mover_settings WHERE array_id=?1",
        "DELETE FROM nas_elastic_folder_cache WHERE array_id=?1",
        "DELETE FROM nas_elastic_operations WHERE array_id=?1",
    ] {
        tx.execute(statement, params![array_id])?;
    }
    let removed = tx.execute(
        "DELETE FROM nas_elastic_arrays WHERE array_id=?1 AND org_id=?2 AND addon_id=?3",
        params![array_id, owner.org_id, owner.addon_id],
    )?;
    tx.commit()?;
    Ok(removed == 1)
}

/// `(array_id, name)` of every array this node has a row for, whatever the
/// owner and whatever the state of the row.
///
/// The import scan needs exactly this and not `elastic_arrays`: an array whose
/// persisted intention no longer reads back fails that function entirely, and
/// answering "unknown" for such an array would offer to adopt storage this
/// node already has a row for — the one thing the `disk_id`/`expected_uuid`
/// uniqueness is there to prevent. Every owner is included for the same
/// reason: another addon's row still holds the disks.
pub fn elastic_array_identities(pool: &DbPool) -> Result<Vec<(String, String)>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let rows = conn
        .prepare("SELECT array_id,name FROM nas_elastic_arrays ORDER BY name")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Adopts an array whose journal this node holds but whose database record it
/// has lost. `spec` is the journal's own intention with the owner ALREADY
/// rewritten by the helper — the re-owning is the point of the operation, and
/// the row is written under `spec.owner`, never under `previous`.
///
/// One immediate transaction, and every uniqueness rule the create path relies
/// on is re-checked inside it BEFORE the inserts: `nas_elastic_disks.disk_id`
/// and `expected_uuid` are UNIQUE and the alias table is keyed by
/// `(kind, value)`, so a member some other array already holds must refuse
/// with a sentence naming the disk rather than surface as a raw SQL error.
///
/// The array arrives `active` — it is complete, its disks are verified and its
/// union may well be serving already — with an EMPTY `state_detail`: the
/// Elastic card and the detail screen print that column as text, and an
/// adopted array's origin used to be written there as the previous owner's
/// org/addon ids — machine ids on screen, and possibly another tenant's. The
/// job row already says the array was adopted (its kind is `elastic_import`),
/// and who it was taken from is the node's log (`elastic::import_apply`), not
/// anything this database shows.
pub fn elastic_import(pool: &DbPool, spec: &ElasticCreateSpec, started_by: &str) -> Result<()> {
    spec.validate()?;
    let at = now();
    // The job's one log line: what happened, without an owner. The job-log
    // window and the job row's sub-text both print it.
    let job_log = "the journal and the disks of this array were already on this node and this \
         instance had no record of it; the journal now names this instance as the owner";
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let closing: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM nas_settings WHERE key='elastic_teardown_started')",
        [], |r| r.get(0))?;
    anyhow::ensure!(!closing, "Rozpoczęto usuwanie instancji; odmowa adopcji macierzy");
    let known: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM nas_elastic_arrays WHERE array_id=?1)",
        params![spec.array_id], |r| r.get(0))?;
    anyhow::ensure!(!known, "Ta macierz jest już zapisana w tej instancji");
    let name_taken: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM nas_elastic_arrays WHERE name=?1)",
        params![spec.name], |r| r.get(0))?;
    anyhow::ensure!(!name_taken, "Nazwa macierzy jest już zajęta w tej instancji");
    for (_, _, disk) in elastic_disk_slots(spec)? {
        let claimed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM nas_elastic_disks WHERE disk_id=?1 OR expected_uuid=?2)",
            params![disk.disk_id, disk.expected_uuid], |r| r.get(0))?;
        anyhow::ensure!(!claimed,
            "Dysk {} należy już do innej macierzy tej instancji", disk.disk_id);
        for value in [Some(disk.disk_id.as_str()), disk.wwn.as_deref(), disk.serial.as_deref()]
            .into_iter().flatten() {
            let reserved: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM nas_elastic_disk_aliases WHERE value=?1)",
                params![value], |r| r.get(0))?;
            anyhow::ensure!(!reserved,
                "Identyfikator {value} jest już zarezerwowany przez inną macierz tej instancji");
        }
    }
    let job_id = uuid::Uuid::now_v7().to_string();
    tx.execute("INSERT INTO nas_jobs
        (job_id,kind,subject,status,progress_pct,started_by,started_at,finished_at,log)
        VALUES (?1,'elastic_import',?2,'succeeded',100,?3,?4,?4,?5)",
        params![job_id,spec.name,started_by,at,job_log])?;
    tx.execute("INSERT INTO nas_elastic_arrays
        (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
        VALUES (?1,?2,?3,?4,?5,'active','',?6,?6)",
        params![spec.array_id,spec.owner.org_id,spec.owner.addon_id,spec.name,
            spec.filesystem.as_str(),at])?;
    insert_elastic_disks(&tx, spec)?;
    // An adopted array is a new row to THIS node — its old record, and any
    // schedule an admin set there, is exactly what was lost — so it gets the
    // default a created one gets.
    insert_default_scrub_schedule(&tx, &spec.array_id)?;
    // The origin operation (migration 14): `elastic_spec` reads the persisted
    // intention from it, so an adopted array without this row could not be
    // read back at all.
    tx.execute("INSERT INTO nas_elastic_operations
        (operation_id,array_id,job_id,kind,state,request_json,error,created_at,finished_at)
        VALUES (?1,?2,?3,'import','succeeded',?4,'',?5,?5)",
        params![spec.operation_id,spec.array_id,job_id,serde_json::to_string(spec)?,at])?;
    stamp_elastic_job_org(&tx, &job_id, "elastic_import", &spec.name)?;
    tx.commit()?;
    Ok(())
}

pub fn finish_elastic_operation(pool: &DbPool, owner: &ElasticOwner, operation_id: &str,
    result: Result<&ElasticResult, &str>) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let array_id: String = tx.query_row("SELECT o.array_id FROM nas_elastic_operations o
        JOIN nas_elastic_arrays a ON a.array_id=o.array_id
        WHERE o.operation_id=?1 AND a.org_id=?2 AND a.addon_id=?3 AND o.state='running'",
        params![operation_id,owner.org_id,owner.addon_id], |r| r.get(0))?;
    let spec = elastic_spec(&tx,owner,&array_id)?;
    let (success, json, detail) = match result {
        Ok(observed) => {
            // A Restore over an unfinished add answers with the add's disk
            // counted (K5).
            let answered = super::elastic::answered_spec(&spec, pinned_add(&tx, &array_id)?.as_ref(), observed)?;
            super::elastic::validate_result(&answered, observed)?;
            (observed.stage == tentanas_helper::elastic::ElasticStage::Ready,
                Some(serde_json::to_string(observed)?), observed.detail.clone().unwrap_or_default())
        }
        Err(detail) => (false,None,detail.to_string()),
    };
    let at = now();
    // A Restore refused because another Elastic command held the lock ran
    // nothing: the array keeps its state and a later Restore is asked again.
    let kind: String = tx.query_row("SELECT kind FROM nas_elastic_operations WHERE operation_id=?1",
        params![operation_id], |r| r.get(0))?;
    if kind == "restore" && result.is_err() && nothing_ran(Some(&detail)) {
        tx.execute("UPDATE nas_elastic_operations SET state='failed',error=?2,finished_at=?3 WHERE operation_id=?1",
            params![operation_id,detail,at])?;
        tx.commit()?;
        return Ok(());
    }
    tx.execute("UPDATE nas_elastic_operations SET state=?2,result_json=?3,error=?4,finished_at=?5
        WHERE operation_id=?1",params![operation_id,if success {"succeeded"} else {"needs_attention"},json,detail,at])?;
    tx.execute("UPDATE nas_elastic_arrays SET state=?2,state_detail=?3,updated_at=?4 WHERE array_id=?1",
        params![array_id,if success {"active"} else {"needs_attention"},detail,at])?;
    tx.commit()?;
    Ok(())
}

pub fn fail_elastic_job(pool: &DbPool, job_id: &str, detail: &str) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("UPDATE nas_elastic_arrays SET state='needs_attention',state_detail=?2,updated_at=?3
        WHERE array_id IN (SELECT array_id FROM nas_elastic_operations WHERE job_id=?1 AND state='running')",
        params![job_id,detail,now()])?;
    tx.execute("UPDATE nas_elastic_operations SET state='needs_attention',error=?2,finished_at=?3
        WHERE job_id=?1 AND state='running'",params![job_id,detail,now()])?;
    tx.commit()?;
    Ok(())
}

pub fn block_elastic_teardown(pool: &DbPool) -> Result<()> {
    let mut conn=write(pool)?;
    let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let reserved:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM nas_elastic_arrays)",[],|r|r.get(0))?;
    anyhow::ensure!(!reserved,"Instancja ma trwałe rezerwacje Elastic; usunięcie utraciłoby nadzór i konfigurację macierzy");
    tx.execute("INSERT INTO nas_settings(key,value,updated_at) VALUES ('elastic_teardown_started','true',?1)
        ON CONFLICT(key) DO NOTHING",params![now()])?;
    tx.commit()?;
    Ok(())
}

// ----- schedules ----------------------------------------------------------------

/// One recurring task of one pool, as the Tasks tab and the pool card show it:
/// the scrub (§5.2) and the TRIM (§5.10) have the same row shape, so they share
/// one reader and differ only in the table the row lives in.
pub struct PoolScheduleRow {
    pub pool: String,
    pub enabled: bool,
    pub schedule: NasSchedule,
    pub last_run_at: Option<String>,
    pub last_result: String,
    pub next_run_at: Option<String>,
}

/// The two tables `PoolTask` names. A table name is interpolated into SQL, so
/// it comes from this enum and never from the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolTask {
    Scrub,
    Trim,
}

impl PoolTask {
    fn table(self) -> &'static str {
        match self {
            Self::Scrub => "nas_scrub_schedules",
            Self::Trim => "nas_trim_schedules",
        }
    }

    /// The `kind` the protocol's schedule rows carry.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Scrub => "scrub",
            Self::Trim => "trim",
        }
    }
}

fn pool_schedule_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PoolScheduleRow> {
    let json: String = r.get(2)?;
    Ok(PoolScheduleRow {
        pool: r.get(0)?,
        enabled: r.get::<_, i64>(1)? != 0,
        schedule: serde_json::from_str(&json).unwrap_or_default(),
        last_run_at: r.get(3)?,
        last_result: r.get(4)?,
        next_run_at: r.get(5)?,
    })
}

const SCRUB_COLUMNS: &str = "pool, enabled, schedule_json, last_run_at, last_result, next_run_at";

pub fn pool_schedule(
    pool: &DbPool,
    task: PoolTask,
    name: &str,
) -> Result<Option<PoolScheduleRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!(
                "SELECT {SCRUB_COLUMNS} FROM {} WHERE pool = ?1",
                task.table()
            ),
            params![name],
            pool_schedule_from_row,
        )
        .optional()?)
}

pub fn list_pool_schedules(pool: &DbPool, task: PoolTask) -> Result<Vec<PoolScheduleRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {SCRUB_COLUMNS} FROM {} ORDER BY pool",
        task.table()
    ))?;
    let rows = stmt
        .query_map([], pool_schedule_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn set_pool_schedule(
    pool: &DbPool,
    task: PoolTask,
    name: &str,
    enabled: bool,
    schedule: &NasSchedule,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        &format!(
            "INSERT INTO {} (pool, enabled, schedule_json, next_run_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(pool) DO UPDATE SET
                enabled = excluded.enabled,
                schedule_json = excluded.schedule_json,
                next_run_at = excluded.next_run_at",
            task.table()
        ),
        params![
            name,
            i64::from(enabled),
            serde_json::to_string(schedule)?,
            next_run_at
        ],
    )?;
    Ok(())
}

/// Gives a schedule row its first `next_run_at`, and ONLY that — the
/// scheduler's first tick over a row nobody has armed yet.
///
/// WHY not the upsert (`set_pool_schedule` & co.): the tick read the row a
/// moment earlier, and writing that copy back rewrote `enabled` and the
/// cadence as they were THEN. An admin who switched the schedule off (or
/// changed it, or — for snapshots — deleted it) in between had the change
/// silently undone: re-enabled, the old cadence restored, a deleted row
/// re-inserted. So this is a compare-and-set against the row as it is NOW:
/// it reads the row's current cadence, computes the slot from THAT with
/// `next_of`, and writes only while the row is still enabled, still unarmed
/// and still on the cadence the slot was computed from (the very text just
/// read, so no re-serialisation can make it differ). Returns whether it armed.
///
/// `key` is the row's WHERE over `?1..?n`, bound from `key_params`.
fn arm_schedule_row(
    pool: &DbPool,
    table: &str,
    key: &str,
    key_params: &[&dyn rusqlite::ToSql],
    next_of: impl FnOnce(&NasSchedule) -> Option<String>,
) -> Result<bool> {
    let conn = write(pool)?;
    let json: Option<String> = conn
        .query_row(
            &format!(
                "SELECT schedule_json FROM {table}
                 WHERE {key} AND enabled = 1 AND next_run_at IS NULL"
            ),
            key_params,
            |r| r.get(0),
        )
        .optional()?;
    let Some(json) = json else {
        return Ok(false);
    };
    let schedule: NasSchedule = serde_json::from_str(&json).unwrap_or_default();
    let Some(next) = next_of(&schedule) else {
        return Ok(false);
    };
    let (next_at, json_at) = (key_params.len() + 1, key_params.len() + 2);
    let mut bound: Vec<&dyn rusqlite::ToSql> = key_params.to_vec();
    bound.push(&next);
    bound.push(&json);
    let armed = conn.execute(
        &format!(
            "UPDATE {table} SET next_run_at = ?{next_at}
             WHERE {key} AND enabled = 1 AND next_run_at IS NULL AND schedule_json = ?{json_at}"
        ),
        bound.as_slice(),
    )?;
    Ok(armed == 1)
}

/// `arm_schedule_row` for a pool's scrub or trim.
pub fn arm_pool_schedule(
    pool: &DbPool,
    task: PoolTask,
    name: &str,
    next_of: impl FnOnce(&NasSchedule) -> Option<String>,
) -> Result<bool> {
    arm_schedule_row(pool, task.table(), "pool = ?1", &[&name as &dyn rusqlite::ToSql], next_of)
}

/// `arm_schedule_row` for one of an Elastic Array's cadences.
pub fn arm_elastic_schedule(
    pool: &DbPool,
    array_id: &str,
    task: ElasticTask,
    next_of: impl FnOnce(&NasSchedule) -> Option<String>,
) -> Result<bool> {
    arm_schedule_row(
        pool,
        "nas_elastic_schedules",
        "array_id = ?1 AND kind = ?2",
        &[&array_id as &dyn rusqlite::ToSql, &task.kind() as &dyn rusqlite::ToSql],
        next_of,
    )
}

/// `arm_schedule_row` for a snapshot schedule.
pub fn arm_snapshot_schedule(
    pool: &DbPool,
    schedule_id: &str,
    next_of: impl FnOnce(&NasSchedule) -> Option<String>,
) -> Result<bool> {
    arm_schedule_row(
        pool,
        "nas_snapshot_schedules",
        "schedule_id = ?1",
        &[&schedule_id as &dyn rusqlite::ToSql],
        next_of,
    )
}

pub fn record_pool_schedule_run(
    pool: &DbPool,
    task: PoolTask,
    name: &str,
    result: &str,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        &format!(
            "UPDATE {} SET last_run_at = ?2, last_result = ?3, next_run_at = ?4
             WHERE pool = ?1",
            task.table()
        ),
        params![name, now(), result, next_run_at],
    )?;
    Ok(())
}

/// Drops the schedules of a pool that no longer exists (destroyed or exported).
/// Both tasks at once: a destroyed pool has no scrub AND no trim left to run.
pub fn delete_pool_schedules(pool: &DbPool, name: &str) -> Result<()> {
    let conn = write(pool)?;
    for task in [PoolTask::Scrub, PoolTask::Trim] {
        conn.execute(
            &format!("DELETE FROM {} WHERE pool = ?1", task.table()),
            params![name],
        )?;
    }
    Ok(())
}

// ----- Elastic schedules and mover settings (§5.3, E2-10) -----------------------

/// The three recurring things an Elastic Array does on a clock. Unlike
/// `PoolTask` this names a ROW and not a table: all three live in
/// `nas_elastic_schedules`, and the kind travels as a bound parameter, so
/// nothing here is ever interpolated into SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElasticTask {
    /// Cache → data disks, optionally with the coupled sync behind it.
    Mover,
    /// The nightly safety net, independent of the mover's coupled sync.
    Sync,
    Scrub,
}

impl ElasticTask {
    /// The `kind` column, and the `kind` the protocol's schedule rows carry.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Mover => "mover",
            Self::Sync => "sync",
            Self::Scrub => "scrub",
        }
    }

    /// Every task, for the loops that must not silently skip one.
    pub const ALL: [Self; 3] = [Self::Mover, Self::Sync, Self::Scrub];
}

/// One recurring Elastic task of one array — the same shape `PoolScheduleRow`
/// has, keyed by the array instead of the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElasticScheduleRow {
    pub array_id: String,
    pub kind: String,
    pub enabled: bool,
    pub schedule: NasSchedule,
    pub last_run_at: Option<String>,
    pub last_result: String,
    pub next_run_at: Option<String>,
}

const ELASTIC_SCHEDULE_COLUMNS: &str =
    "array_id, kind, enabled, schedule_json, last_run_at, last_result, next_run_at";

fn elastic_schedule_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ElasticScheduleRow> {
    let json: String = r.get(3)?;
    Ok(ElasticScheduleRow {
        array_id: r.get(0)?,
        kind: r.get(1)?,
        enabled: r.get::<_, i64>(2)? != 0,
        schedule: serde_json::from_str(&json).unwrap_or_default(),
        last_run_at: r.get(4)?,
        last_result: r.get(5)?,
        next_run_at: r.get(6)?,
    })
}

/// Every array's schedule of one kind, for the scheduler's sweep.
pub fn list_elastic_schedules(pool: &DbPool, task: ElasticTask) -> Result<Vec<ElasticScheduleRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ELASTIC_SCHEDULE_COLUMNS} FROM nas_elastic_schedules
         WHERE kind = ?1 ORDER BY array_id"
    ))?;
    let rows = stmt
        .query_map(params![task.kind()], elastic_schedule_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn elastic_schedule(
    pool: &DbPool,
    array_id: &str,
    task: ElasticTask,
) -> Result<Option<ElasticScheduleRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!(
                "SELECT {ELASTIC_SCHEDULE_COLUMNS} FROM nas_elastic_schedules
                 WHERE array_id = ?1 AND kind = ?2"
            ),
            params![array_id, task.kind()],
            elastic_schedule_from_row,
        )
        .optional()?)
}

pub fn set_elastic_schedule(
    pool: &DbPool,
    array_id: &str,
    task: ElasticTask,
    enabled: bool,
    schedule: &NasSchedule,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_elastic_schedules (array_id, kind, enabled, schedule_json, next_run_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(array_id, kind) DO UPDATE SET
            enabled = excluded.enabled,
            schedule_json = excluded.schedule_json,
            next_run_at = excluded.next_run_at",
        params![
            array_id,
            task.kind(),
            i64::from(enabled),
            serde_json::to_string(schedule)?,
            next_run_at
        ],
    )?;
    Ok(())
}

/// The default scrub cadence as the struct the protocol carries — the value of
/// `default_elastic_scrub_schedule_json!`.
///
/// MONTHLY, as the owner decided; the rest is chosen against what the product
/// already schedules:
/// - 04:00 local: the hour n11's scrub dialog proposes (`elastic-detail.js`
///   `scrub: { hour: 4 }`), one hour after the 03:00 it proposes for a sync —
///   the quiet part of the night, and not on top of a sync an admin arms.
/// - the 15th, not the 1st: the Tasks tab proposes the monthly LONG SMART
///   self-test for the 1st at 04:00 (`tasks.js`). Both read every sector of
///   the same disks; stacked, each slows the other and a long test can be
///   aborted by the load. Half a month apart they never meet.
/// - `weekday` is unused by a monthly cadence; 0 is what every other default
///   in the product carries there.
///
/// No scrub percentage or age goes with it: the product runs every scrub as
/// `snapraid -p full scrub` (`tentanas_helper::elastic::snapraid_args`), so
/// `scrub_percent` / `scrub_older_than_days` are not arguments of any run, and
/// a FULL monthly scrub is what reads every block once a month. It is also the
/// run that settles an earlier unresolved scrub or repair
/// (`UNRESOLVED_ELASTIC_OPERATION`), which a partial one could not.
pub fn default_elastic_scrub_schedule() -> NasSchedule {
    NasSchedule {
        every: "monthly".to_string(),
        hour: 4,
        minute: 0,
        weekday: 0,
        day: 15,
    }
}

/// Gives one array its default scrub, inside the caller's transaction — the
/// one that writes the array row and its member rows, which must already be
/// in it: the parity check below reads them.
///
/// `INSERT OR IGNORE`, never an upsert: whatever scrub row the array already
/// has is somebody's decision and is left alone. Nothing but the array row's
/// own creation calls this, so a schedule an admin disables is never
/// re-armed; there is no path that deletes one short of dissolving the array
/// (`delete_elastic_array`).
///
/// An array without parity gets nothing — see migration 19.
fn insert_default_scrub_schedule(tx: &Connection, array_id: &str) -> Result<()> {
    tx.execute(
        concat!(
            "INSERT OR IGNORE INTO nas_elastic_schedules (array_id, kind, enabled, schedule_json)
             SELECT ?1, ?2, 1, '",
            default_elastic_scrub_schedule_json!(),
            "'
              WHERE EXISTS (SELECT 1 FROM nas_elastic_disks
                             WHERE array_id = ?1 AND role = 'parity')"
        ),
        params![array_id, ElasticTask::Scrub.kind()],
    )?;
    Ok(())
}

pub fn record_elastic_schedule_run(
    pool: &DbPool,
    array_id: &str,
    task: ElasticTask,
    result: &str,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_elastic_schedules SET last_run_at = ?3, last_result = ?4, next_run_at = ?5
         WHERE array_id = ?1 AND kind = ?2",
        params![array_id, task.kind(), now(), result, next_run_at],
    )?;
    Ok(())
}

/// The mover settings an admin CHOSE for this array, or `None` when nobody
/// has. `None` is what makes `MoverConfig::configured` false, and it is the
/// difference between a default a run falls back on and a decision.
pub fn mover_settings(pool: &DbPool, array_id: &str) -> Result<Option<(u64, u8, bool)>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(mover_settings_on(&conn, array_id)?)
}

fn mover_settings_on(conn: &Connection, array_id: &str) -> rusqlite::Result<Option<(u64, u8, bool)>> {
    conn.query_row(
        "SELECT min_age_secs, cache_min_free_pct, coupled_sync
         FROM nas_elastic_mover_settings WHERE array_id = ?1",
        params![array_id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?.max(0) as u64,
                r.get::<_, i64>(1)?.clamp(0, 100) as u8,
                r.get::<_, i64>(2)? != 0,
            ))
        },
    )
    .optional()
}

pub fn set_mover_settings(
    pool: &DbPool,
    array_id: &str,
    min_age_secs: u64,
    cache_min_free_pct: u8,
    coupled_sync: bool,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_elastic_mover_settings
            (array_id, min_age_secs, cache_min_free_pct, coupled_sync)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(array_id) DO UPDATE SET
            min_age_secs = excluded.min_age_secs,
            cache_min_free_pct = excluded.cache_min_free_pct,
            coupled_sync = excluded.coupled_sync",
        params![
            array_id,
            i64::try_from(min_age_secs).unwrap_or(i64::MAX),
            i64::from(cache_min_free_pct),
            i64::from(coupled_sync)
        ],
    )?;
    Ok(())
}

/// How many folders of ONE array may carry a policy of their own.
///
/// It is the helper's own ceiling (`MoverRules::validate` refuses more than
/// 128 folder rules), enforced HERE so an admin is refused at the row they are
/// editing instead of by the mover run months later, on a request that names
/// no folder at all.
pub const FOLDER_POLICY_LIMIT: i64 = 128;

/// The cache policies this node has STORED for one array, keyed by folder.
///
/// Only the exceptions are in here: 'yes' is the default and is spelled by the
/// absence of a row, so a folder missing from this map is a folder the age and
/// free-space rules decide for.
fn elastic_folder_policies_on(
    conn: &Connection,
    array_id: &str,
) -> rusqlite::Result<BTreeMap<String, String>> {
    conn.prepare(
        "SELECT folder, cache_policy FROM nas_elastic_folder_cache
         WHERE array_id = ?1 ORDER BY folder",
    )?
    .query_map(params![array_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?
    .collect()
}

/// The share serving each folder of one union, keyed by folder name.
///
/// A share names the union path and a folder under it (`shares.rs` builds it
/// with `union_path()`), so the folder is the FIRST segment after the union
/// and a share pointing deeper belongs to the folder it sits inside. The
/// union path is bound, never interpolated: it comes from an array name the
/// helper validated, and the one place that could get that wrong should not
/// be this query.
fn folder_shares_on(
    conn: &Connection,
    union: &str,
) -> rusqlite::Result<BTreeMap<String, (String, String)>> {
    let mut shares = BTreeMap::new();
    let mut statement = conn.prepare(
        "SELECT share_id, name, source_path FROM nas_shares
         WHERE source_path LIKE ?1 ESCAPE '\\' ORDER BY name",
    )?;
    let prefix = format!("{union}/");
    let pattern = format!(
        "{}%",
        prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    let rows = statement.query_map(params![pattern], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (share_id, name, source_path) = row?;
        let Some(rest) = source_path.strip_prefix(&prefix) else {
            continue;
        };
        let folder = rest.split('/').next().unwrap_or_default();
        if folder.is_empty() {
            continue;
        }
        // First share wins, ordered by name, so the same array always reports
        // the same one rather than whichever row the planner returned last.
        shares.entry(folder.to_string()).or_insert((share_id, name));
    }
    Ok(shares)
}

/// Stores one folder's cache policy, or RETURNS IT TO THE DEFAULT.
///
/// `CachePolicy::Yes` deletes the row: the default is the absence of one, so
/// writing 'yes' would create a second spelling of the same state.
///
/// Answers `false` when `FOLDER_POLICY_LIMIT` refused the write and NOTHING
/// was written — a refusal the caller turns into a sentence, kept apart from
/// the `Err` of a database that is actually broken. The bound is checked
/// inside the transaction that writes: counting on one connection and writing
/// on another is how a limit gets exceeded by exactly the two admins who were
/// both told they were under it.
pub fn set_elastic_folder_policy(
    pool: &DbPool,
    array_id: &str,
    folder: &str,
    policy: super::elastic::CachePolicy,
) -> Result<bool> {
    let mut conn = write(pool)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if policy == super::elastic::CachePolicy::Yes {
        tx.execute(
            "DELETE FROM nas_elastic_folder_cache WHERE array_id=?1 AND folder=?2",
            params![array_id, folder],
        )?;
        tx.commit()?;
        return Ok(true);
    }
    let stored: i64 = tx.query_row(
        "SELECT COUNT(*) FROM nas_elastic_folder_cache WHERE array_id=?1 AND folder<>?2",
        params![array_id, folder],
        |r| r.get(0),
    )?;
    if stored >= FOLDER_POLICY_LIMIT {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO nas_elastic_folder_cache (array_id, folder, cache_policy)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(array_id, folder) DO UPDATE SET cache_policy = excluded.cache_policy",
        params![array_id, folder, policy.as_str()],
    )?;
    tx.commit()?;
    Ok(true)
}

/// The persisted mover configuration of one array, read on a connection the
/// caller already holds. The cadence and the switch come from the schedule
/// row, the three rules from the settings row, and each half is absent on its
/// own — a config import that wrote only a cadence must not make the rules
/// read as chosen.
fn elastic_mover_config(
    conn: &Connection,
    array_id: &str,
) -> rusqlite::Result<super::elastic::MoverConfig> {
    let defaults = super::elastic::MoverConfig::default();
    let schedule = conn
        .query_row(
            &format!(
                "SELECT {ELASTIC_SCHEDULE_COLUMNS} FROM nas_elastic_schedules
                 WHERE array_id = ?1 AND kind = ?2"
            ),
            // Bound, not inlined — the same rule the migration comment states,
            // and the reason this table can be shared by three kinds at all.
            params![array_id, ElasticTask::Mover.kind()],
            elastic_schedule_from_row,
        )
        .optional()?;
    let settings = mover_settings_on(conn, array_id)?;
    Ok(super::elastic::MoverConfig {
        // No schedule row means no restriction: files move whenever they are
        // old enough. A row switched on confines those moves to its slots.
        schedule_enabled: schedule.as_ref().is_some_and(|s| s.enabled),
        schedule: schedule.map(|s| s.schedule),
        min_age_secs: settings.map(|s| s.0).unwrap_or(defaults.min_age_secs),
        cache_min_free_pct: settings.map(|s| s.1).unwrap_or(defaults.cache_min_free_pct),
        coupled_sync: settings.map(|s| s.2).unwrap_or(defaults.coupled_sync),
        configured: settings.is_some(),
    })
}

/// The persisted SnapRAID cadences of one array. The scrub percentage and age
/// stay where they were — E2-10 persists schedules, not the scrub tuning.
fn elastic_snapraid_config(
    conn: &Connection,
    array_id: &str,
) -> rusqlite::Result<super::elastic::SnapraidConfig> {
    let mut config = super::elastic::SnapraidConfig::default();
    for task in [ElasticTask::Sync, ElasticTask::Scrub] {
        let row = conn
            .query_row(
                &format!(
                    "SELECT {ELASTIC_SCHEDULE_COLUMNS} FROM nas_elastic_schedules
                     WHERE array_id = ?1 AND kind = ?2"
                ),
                params![array_id, task.kind()],
                elastic_schedule_from_row,
            )
            .optional()?;
        // A disabled cadence is still CARRIED — it is saved, it just does not
        // fire, which is what `schedule.enabled_sub` promises the admin — so
        // the switch travels beside it rather than in place of it.
        let enabled = row.as_ref().is_some_and(|r| r.enabled);
        let schedule = row.map(|r| r.schedule);
        if task == ElasticTask::Sync {
            config.sync_schedule = schedule;
            config.sync_enabled = enabled;
        } else {
            config.scrub_schedule = schedule;
            config.scrub_enabled = enabled;
        }
    }
    Ok(config)
}

/// Every array of every owner on this node, for the scheduler — which has a
/// database and no request, so it cannot be handed an `ElasticOwner` the way
/// a handler is.
pub fn elastic_arrays_all(pool: &DbPool) -> Result<Vec<super::elastic::ElasticArrayRow>> {
    let owners = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT org_id, addon_id FROM nas_elastic_arrays ORDER BY org_id, addon_id",
        )?;
        let owners = stmt
            .query_map([], |r| {
                Ok(ElasticOwner {
                    org_id: r.get(0)?,
                    addon_id: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        owners
    };
    let mut arrays = Vec::new();
    for owner in owners {
        arrays.extend(elastic_arrays(pool, &owner)?);
    }
    Ok(arrays)
}

fn snapshot_schedule_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NasSnapshotSchedule> {
    let json: String = r.get(4)?;
    Ok(NasSnapshotSchedule {
        schedule_id: r.get(0)?,
        dataset: r.get(1)?,
        enabled: r.get::<_, i64>(2)? != 0,
        recursive: r.get::<_, i64>(3)? != 0,
        schedule: serde_json::from_str(&json).unwrap_or_default(),
        keep_frequent: r.get::<_, i64>(5)? as u32,
        keep_hourly: r.get::<_, i64>(6)? as u32,
        keep_daily: r.get::<_, i64>(7)? as u32,
        keep_weekly: r.get::<_, i64>(8)? as u32,
        keep_monthly: r.get::<_, i64>(9)? as u32,
        last_run_at: r.get(10)?,
        next_run_at: r.get(11)?,
        // Filled from the live snapshot list by the caller that has it.
        snapshot_count: 0,
        protect_days: r.get::<_, i64>(12)? as u32,
    })
}

const SNAPSHOT_SCHEDULE_COLUMNS: &str =
    "schedule_id, dataset, enabled, recursive, schedule_json, keep_frequent, keep_hourly, \
     keep_daily, keep_weekly, keep_monthly, last_run_at, next_run_at, protect_days";

pub fn list_snapshot_schedules(pool: &DbPool) -> Result<Vec<NasSnapshotSchedule>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {SNAPSHOT_SCHEDULE_COLUMNS} FROM nas_snapshot_schedules ORDER BY dataset"
    ))?;
    let rows = stmt
        .query_map([], snapshot_schedule_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn snapshot_schedule(pool: &DbPool, schedule_id: &str) -> Result<Option<NasSnapshotSchedule>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!(
                "SELECT {SNAPSHOT_SCHEDULE_COLUMNS} FROM nas_snapshot_schedules \
                 WHERE schedule_id = ?1"
            ),
            params![schedule_id],
            snapshot_schedule_from_row,
        )
        .optional()?)
}

/// One schedule per dataset: the unique index enforces it, and setting a
/// schedule for a dataset that already has one replaces it in place.
pub fn upsert_snapshot_schedule(
    pool: &DbPool,
    schedule: &NasSnapshotSchedule,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_snapshot_schedules
            (schedule_id, dataset, enabled, recursive, schedule_json, keep_frequent, keep_hourly,
             keep_daily, keep_weekly, keep_monthly, next_run_at, protect_days)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(dataset) DO UPDATE SET
            enabled = excluded.enabled, recursive = excluded.recursive,
            schedule_json = excluded.schedule_json, keep_frequent = excluded.keep_frequent,
            keep_hourly = excluded.keep_hourly, keep_daily = excluded.keep_daily,
            keep_weekly = excluded.keep_weekly, keep_monthly = excluded.keep_monthly,
            next_run_at = excluded.next_run_at, protect_days = excluded.protect_days",
        params![
            schedule.schedule_id,
            schedule.dataset,
            i64::from(schedule.enabled),
            i64::from(schedule.recursive),
            serde_json::to_string(&schedule.schedule)?,
            i64::from(schedule.keep_frequent),
            i64::from(schedule.keep_hourly),
            i64::from(schedule.keep_daily),
            i64::from(schedule.keep_weekly),
            i64::from(schedule.keep_monthly),
            next_run_at,
            i64::from(schedule.protect_days)
        ],
    )?;
    Ok(())
}

pub fn delete_snapshot_schedule(pool: &DbPool, schedule_id: &str) -> Result<bool> {
    let conn = write(pool)?;
    Ok(conn.execute(
        "DELETE FROM nas_snapshot_schedules WHERE schedule_id = ?1",
        params![schedule_id],
    )? == 1)
}

pub fn record_snapshot_run(
    pool: &DbPool,
    schedule_id: &str,
    result: &str,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_snapshot_schedules SET last_run_at = ?2, last_result = ?3, next_run_at = ?4
         WHERE schedule_id = ?1",
        params![schedule_id, now(), result, next_run_at],
    )?;
    Ok(())
}

/// Records that `snapshot` was held for `protect_days` days. `protected_until`
/// is what the UI shows: the day the admin asked protection to last until, NOT
/// a moment ZFS enforces — the hold stays until a four-eyes approval releases
/// it, which is the only path this app has (§5.10).
pub fn record_snapshot_protection(
    pool: &DbPool,
    snapshot: &str,
    protect_days: u32,
    protected_until: &str,
    protected_by: &str,
    recursive: bool,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_snapshot_protection
            (snapshot, protect_days, protected_until, protected_by, protected_at, recursive)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(snapshot) DO UPDATE SET
            protect_days = excluded.protect_days,
            protected_until = excluded.protected_until,
            protected_by = excluded.protected_by,
            protected_at = excluded.protected_at,
            recursive = excluded.recursive",
        params![
            snapshot,
            i64::from(protect_days),
            protected_until,
            protected_by,
            now(),
            i64::from(recursive)
        ],
    )?;
    Ok(())
}

/// Whether the app placed this snapshot's hold recursively. `false` for a
/// snapshot it never recorded — a hold somebody put there by hand is released
/// exactly as narrowly as the app knows how, and the release job verifies the
/// result instead of assuming it.
pub fn snapshot_protection_recursive(pool: &DbPool, snapshot: &str) -> Result<bool> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT recursive FROM nas_snapshot_protection WHERE snapshot = ?1",
            params![snapshot],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0)
        != 0)
}

/// Every protection record, as `snapshot -> protected_until`. The snapshot
/// list joins it; a record whose snapshot is gone simply never matches.
pub fn snapshot_protection(pool: &DbPool) -> Result<std::collections::HashMap<String, String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached("SELECT snapshot, protected_until FROM nas_snapshot_protection")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().collect())
}

/// Forgets the app's record of a protection. Called when an approved release
/// really took the hold off — leaving the row would make the UI claim a
/// protection ZFS no longer has.
pub fn forget_snapshot_protection(pool: &DbPool, snapshot: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "DELETE FROM nas_snapshot_protection WHERE snapshot = ?1",
        params![snapshot],
    )?;
    Ok(())
}

// ----- four eyes (§5.10) -------------------------------------------------------

/// A parked operation with the two things the wire row does not carry: the
/// request to replay on approval, and the instance it belongs to.
#[derive(Debug, Clone)]
pub struct ApprovalRow {
    pub approval: tentaflow_protocol::tentanas::NasPendingApproval,
    pub payload_json: String,
    pub org_id: String,
    pub addon_id: String,
}

const APPROVAL_COLUMNS: &str = "request_id, operation, subject, detail, payload_json, status, \
                                org_id, addon_id, requested_by, requested_at, expires_at, \
                                decided_by, decided_at, decision_note, decision_job_id";

fn approval_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ApprovalRow> {
    Ok(ApprovalRow {
        approval: tentaflow_protocol::tentanas::NasPendingApproval {
            request_id: r.get(0)?,
            operation: r.get(1)?,
            subject: r.get(2)?,
            detail: r.get(3)?,
            status: r.get(5)?,
            requested_by: r.get(8)?,
            requested_at: r.get(9)?,
            expires_at: r.get(10)?,
            decided_by: r.get(11)?,
            decided_at: r.get(12)?,
            decision_note: r.get(13)?,
            decision_job_id: r.get(14)?,
            // Only the handler knows who is asking.
            is_own_request: false,
        },
        payload_json: r.get(4)?,
        org_id: r.get(6)?,
        addon_id: r.get(7)?,
    })
}

pub fn insert_approval(pool: &DbPool, row: &ApprovalRow) -> Result<()> {
    let a = &row.approval;
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_pending_approvals
            (request_id, operation, subject, detail, payload_json, status, org_id, addon_id,
             requested_by, requested_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            a.request_id,
            a.operation,
            a.subject,
            a.detail,
            row.payload_json,
            a.status,
            row.org_id,
            row.addon_id,
            a.requested_by,
            a.requested_at,
            a.expires_at
        ],
    )?;
    Ok(())
}

pub fn approval(pool: &DbPool, request_id: &str) -> Result<Option<ApprovalRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!("SELECT {APPROVAL_COLUMNS} FROM nas_pending_approvals WHERE request_id = ?1"),
            params![request_id],
            approval_from_row,
        )
        .optional()?)
}

/// One organisation's approvals, newest first: the open ones, plus — with
/// `include_closed` — the decided and expired ones, so the list can show
/// what happened to a request. The organisation is part of the QUERY, not a
/// filter applied afterwards: the list is capped, and with the cap first
/// another tenant's requests could fill it and push the caller's own out of
/// view. An empty `org_id` owns nothing and gets nothing.
pub fn list_approvals(pool: &DbPool, org_id: &str, include_closed: bool) -> Result<Vec<ApprovalRow>> {
    if org_id.is_empty() {
        return Ok(Vec::new());
    }
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let sql = format!(
        "SELECT {APPROVAL_COLUMNS} FROM nas_pending_approvals WHERE org_id = ?1 {} \
         ORDER BY requested_at DESC LIMIT 200",
        if include_closed { "" } else { "AND status = 'pending'" }
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![org_id], approval_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Moves one operation out of 'pending' and returns whether THIS call did it.
/// The `status = 'pending'` guard is what makes an approved operation execute
/// exactly once: a second decision, from a retry or a second tab, changes no
/// rows and is refused by the caller.
pub fn close_approval(
    pool: &DbPool,
    request_id: &str,
    status: &str,
    decided_by: Option<&str>,
    note: &str,
) -> Result<bool> {
    let conn = write(pool)?;
    let changed = conn.execute(
        "UPDATE nas_pending_approvals
            SET status = ?2, decided_by = ?3, decided_at = ?4, decision_note = ?5
          WHERE request_id = ?1 AND status = 'pending'",
        params![request_id, status, decided_by, now(), note],
    )?;
    Ok(changed == 1)
}

/// Records what the approved operation started, or that it failed to start.
pub fn set_approval_outcome(
    pool: &DbPool,
    request_id: &str,
    status: &str,
    job_id: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_pending_approvals SET status = ?2, decision_job_id = ?3 WHERE request_id = ?1",
        params![request_id, status, job_id],
    )?;
    Ok(())
}

/// The pending operations whose TTL has passed at `now` (RFC 3339 UTC).
pub fn approvals_past_ttl(pool: &DbPool, now: &str) -> Result<Vec<ApprovalRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {APPROVAL_COLUMNS} FROM nas_pending_approvals \
         WHERE status = 'pending' AND expires_at <= ?1"
    ))?;
    let rows = stmt
        .query_map(params![now], approval_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn snapshot_schedule_result(pool: &DbPool, schedule_id: &str) -> Result<String> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT last_result FROM nas_snapshot_schedules WHERE schedule_id = ?1",
            params![schedule_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_default())
}

pub fn smart_schedule(pool: &DbPool) -> Result<NasSmartSchedule> {
    Ok(setting(pool, SETTING_SMART_SCHEDULE)?
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default())
}

pub fn set_smart_schedule(pool: &DbPool, schedule: &NasSmartSchedule) -> Result<()> {
    set_setting(pool, SETTING_SMART_SCHEDULE, &serde_json::to_string(schedule)?)
}

// ----- pool samples -------------------------------------------------------------

pub struct PoolSampleInsert<'a> {
    pub pool: &'a str,
    pub sampled_at: &'a str,
    pub read_bps: u64,
    pub write_bps: u64,
    pub read_iops: f64,
    pub write_iops: f64,
    pub read_latency_ms: f64,
    pub write_latency_ms: f64,
}

pub fn insert_pool_samples(pool: &DbPool, samples: &[PoolSampleInsert<'_>]) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO nas_pool_samples
                (pool, sampled_at, read_bps, write_bps, read_iops, write_iops,
                 read_latency_ms, write_latency_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for s in samples {
            stmt.execute(params![
                s.pool,
                s.sampled_at,
                s.read_bps as i64,
                s.write_bps as i64,
                s.read_iops,
                s.write_iops,
                s.read_latency_ms,
                s.write_latency_ms,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Pool throughput history in the shape the disk detail already uses, so the
/// dashboard draws both charts with one component. `temperature_c`,
/// `reallocated_sectors` and `pending_sectors` stay `None`: a pool has no
/// temperature and no sectors of its own — those belong to its member disks,
/// which the Disks tab charts separately. `await_ms` carries the combined
/// read/write service time, weighted by the IOPS of each direction.
pub fn pool_samples_since(pool: &DbPool, name: &str, since: &str) -> Result<Vec<NasDiskSample>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT sampled_at, read_bps, write_bps, read_iops, write_iops,
                read_latency_ms, write_latency_ms
         FROM nas_pool_samples WHERE pool = ?1 AND sampled_at >= ?2 ORDER BY sampled_at",
    )?;
    let rows = stmt
        .query_map(params![name, since], |r| {
            let read_iops: f64 = r.get(3)?;
            let write_iops: f64 = r.get(4)?;
            let read_latency: f64 = r.get(5)?;
            let write_latency: f64 = r.get(6)?;
            let ops = read_iops + write_iops;
            let await_ms = if ops > 0.0 {
                (read_iops * read_latency + write_iops * write_latency) / ops
            } else {
                0.0
            };
            Ok(NasDiskSample {
                at: r.get(0)?,
                temperature_c: None,
                reallocated_sectors: None,
                pending_sectors: None,
                read_bps: r.get::<_, i64>(1)? as u64,
                write_bps: r.get::<_, i64>(2)? as u64,
                await_ms: (await_ms * 100.0).round() / 100.0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Pool samples are the live chart of the pool detail, not a health record:
/// 24 h is everything the view can show, so nothing older is worth its rows.
pub fn prune_pool_samples(pool: &DbPool) -> Result<usize> {
    let conn = write(pool)?;
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(24))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(conn.execute(
        "DELETE FROM nas_pool_samples WHERE sampled_at < ?1",
        params![cutoff],
    )?)
}

// ----- the file access audit (§5.10) ---------------------------------------------

/// How long the access log keeps a row. The same 30 days the disk history
/// keeps, for the same reason: it is the window the UI labels its own view
/// with, and it lives here rather than in three call sites.
pub const ACCESS_LOG_DAYS: u32 = 30;

/// Where the last collection of `vfs_full_audit` lines stopped — journald's
/// own cursor, so the next one continues exactly there.
pub const SETTING_AUDIT_CURSOR: &str = "access_audit_cursor";
/// When the last collection ran and how it went, so the view can say the log
/// is current rather than leaving an empty table ambiguous.
pub const SETTING_AUDIT_COLLECTED_AT: &str = "access_audit_collected_at";
pub const SETTING_AUDIT_STATE: &str = "access_audit_state";
pub const SETTING_AUDIT_DETAIL: &str = "access_audit_detail";
/// The auditd rules document the last successful apply wrote, so an unrelated
/// share edit does not reload the host's audit rules (same trick as the ksmbd
/// and exports documents).
pub const SETTING_AUDIT_RULES: &str = "audit_rules_document";
/// When the forwarder last delivered a batch, and why it last failed.
pub const SETTING_FORWARD_SENT_AT: &str = "forward_last_sent_at";
pub const SETTING_FORWARD_ERROR: &str = "forward_last_error";

const ACCESS_COLUMNS: &str =
    "event_id, at, share, user, client, operation, result, target, detail";

fn access_event_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NasAccessEvent> {
    Ok(NasAccessEvent {
        event_id: r.get::<_, i64>(0)?.max(0) as u64,
        at: r.get(1)?,
        share: r.get(2)?,
        user: r.get(3)?,
        client: r.get(4)?,
        operation: r.get(5)?,
        result: r.get(6)?,
        target: r.get(7)?,
        detail: r.get(8)?,
    })
}

/// Appends one collection's worth of parsed audit lines. One transaction: a
/// partially inserted batch whose cursor was already stored would lose the
/// remainder for good.
///
/// Each line is stamped with the organisation that owns its share AT THE
/// MOMENT IT IS COLLECTED (migration 20), in the INSERT itself: the log then
/// keeps its owner after the share is deleted, and a later share of another
/// organisation that takes the same name never inherits the old lines. A
/// line whose share has no row gets '' and is shown to nobody — the
/// collector only keeps lines of shares it audits, so that is a share deleted
/// between the read and the write, not a normal case.
pub fn insert_access_events(pool: &DbPool, events: &[NasAccessEvent]) -> Result<usize> {
    if events.is_empty() {
        return Ok(0);
    }
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO nas_access_events
                (at, share, user, client, operation, result, target, detail, org_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                     COALESCE((SELECT org_id FROM nas_shares WHERE name = ?2), ''))",
        )?;
        for e in events {
            stmt.execute(params![
                e.at,
                e.share,
                e.user,
                e.client,
                e.operation,
                e.result,
                e.target,
                e.detail
            ])?;
        }
    }
    tx.commit()?;
    Ok(events.len())
}

/// What the view asks for. An empty string matches everything, so one filter
/// shape serves the whole "Dziennik dostępu".
#[derive(Debug, Clone, Default)]
pub struct AccessFilter<'a> {
    pub share: &'a str,
    pub user: &'a str,
    pub operation: &'a str,
    /// 'ok' | 'fail' | '' (both).
    pub result: &'a str,
    pub since: &'a str,
    pub limit: u32,
}

/// The rows of the access log one organisation may read: the lines of its
/// own shares (migration 20). An empty org id matches nothing — never the
/// unowned '' rows — for the same reason `VISIBLE_TO_ORG_SQL` guards it.
const ACCESS_OF_ORG_SQL: &str = "(org_id = ?6 AND ?6 <> '')";

/// The filtered page plus how many rows the filter matched in total, so the
/// view can say "1000 z 4213" instead of pretending the page is everything.
///
/// `org_id` is the asking organisation: the log names shares, accounts,
/// client addresses and file paths, so another tenant's lines are not in the
/// page, not in the total and not in the facets.
pub fn access_events(
    pool: &DbPool,
    org_id: &str,
    filter: &AccessFilter<'_>,
) -> Result<(Vec<NasAccessEvent>, u32)> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    // Every clause is a bound parameter with a fixed SQL shape; only the LIMIT
    // is interpolated, and it is a u32 this function clamps itself.
    let where_sql = format!(
        "WHERE (?1 = '' OR share = ?1)
           AND (?2 = '' OR user = ?2)
           AND (?3 = '' OR operation = ?3)
           AND (?4 = '' OR result = ?4)
           AND (?5 = '' OR at >= ?5)
           AND {ACCESS_OF_ORG_SQL}"
    );
    let args = params![
        filter.share,
        filter.user,
        filter.operation,
        filter.result,
        filter.since,
        org_id
    ];
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM nas_access_events {where_sql}"),
        args,
        |r| r.get(0),
    )?;
    let limit = filter.limit.clamp(1, 5_000);
    let mut stmt = conn.prepare(&format!(
        "SELECT {ACCESS_COLUMNS} FROM nas_access_events {where_sql}
         ORDER BY at DESC, event_id DESC LIMIT {limit}"
    ))?;
    let rows = stmt
        .query_map(args, access_event_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((rows, total.max(0) as u32))
}

/// The distinct shares, users and operations present in the retained window,
/// so the view's filters offer what the node actually logged — for the
/// asking organisation's own lines only: a facet list is as much a leak of
/// another tenant's share and account names as the rows are.
pub fn access_facets(pool: &DbPool, org_id: &str) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut out = Vec::new();
    for column in ["share", "user", "operation"] {
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT DISTINCT {column} FROM nas_access_events
              WHERE {column} <> '' AND org_id = ?1 AND ?1 <> '' ORDER BY {column} LIMIT 200"
        ))?;
        out.push(
            stmt.query_map(params![org_id], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
    }
    let mut it = out.into_iter();
    Ok((
        it.next().unwrap_or_default(),
        it.next().unwrap_or_default(),
        it.next().unwrap_or_default(),
    ))
}

/// How many lines of the log belong to `org_id` — the figure under the
/// audit card, which would otherwise tell a tenant how busy the others are.
pub fn access_event_count(pool: &DbPool, org_id: &str) -> Result<u32> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM nas_access_events WHERE org_id = ?1 AND ?1 <> ''",
        params![org_id],
        |r| r.get::<_, i64>(0),
    )? as u32)
}

/// Retention of the access log: rows older than `ACCESS_LOG_DAYS` go, and so
/// do the oldest rows once the table passes `MAX_ACCESS_ROWS` — a busy share
/// can produce more lines in a day than a node should keep for a month, and a
/// log with no ceiling is how an instance database eats a rootfs.
pub fn prune_access_events(pool: &DbPool) -> Result<usize> {
    const MAX_ACCESS_ROWS: i64 = 500_000;
    let conn = write(pool)?;
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(i64::from(ACCESS_LOG_DAYS)))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut removed = conn.execute(
        "DELETE FROM nas_access_events WHERE at < ?1",
        params![cutoff],
    )?;
    removed += conn.execute(
        "DELETE FROM nas_access_events WHERE event_id <= (
             SELECT MAX(event_id) - ?1 FROM nas_access_events
         )",
        params![MAX_ACCESS_ROWS],
    )?;
    Ok(removed)
}

// ----- forwarding the alert pipeline outwards (§5.9) -------------------------------

/// One row waiting to leave this node: an alert or an audited access, already
/// flattened into what both transports send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardRow {
    /// 'alert' | 'access'.
    pub kind: &'static str,
    pub id: String,
    pub at: String,
    /// 'info' | 'warning' | 'critical'.
    pub severity: String,
    pub subject: String,
    pub summary: String,
    pub detail: String,
}

/// NODE-WIDE alerts (`org_id IS NULL`) that have not been forwarded yet,
/// oldest first. An alert is forwarded when it is RAISED, so a row already
/// acknowledged is still sent: the external collector's job is to see what
/// happened, not what the admin has since read.
///
/// An organisation's alert (an Elastic Array's, a parked request's, or ''
/// for an owner that is gone — migration 18) never reaches the shared
/// target, and the filter is part of THIS query on purpose: the owner is
/// stamped in the statement that inserts the row (`raise_alert`), so no row
/// can be selected here without its owner. Reading the owned ids first and
/// the batch second let an owned alert raised in between slip into the batch.
pub fn unforwarded_alerts(pool: &DbPool, limit: u32) -> Result<Vec<ForwardRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT alert_id, raised_at, severity, subject_kind, subject_id, title, detail
           FROM nas_alerts WHERE forwarded_at IS NULL AND org_id IS NULL
          ORDER BY raised_at LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| {
            Ok(ForwardRow {
                kind: "alert",
                id: r.get(0)?,
                at: r.get(1)?,
                severity: r.get(2)?,
                subject: format!("{}:{}", r.get::<_, String>(3)?, r.get::<_, String>(4)?),
                summary: r.get(5)?,
                detail: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Marks what actually left the node. Called AFTER a successful send, so a
/// crash in between replays a row instead of dropping it.
pub fn mark_forwarded(pool: &DbPool, rows: &[ForwardRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    let stamp = now();
    for row in rows {
        let sql = if row.kind == "alert" {
            "UPDATE nas_alerts SET forwarded_at = ?2 WHERE alert_id = ?1"
        } else {
            "UPDATE nas_access_events SET forwarded_at = ?2 WHERE event_id = ?1"
        };
        tx.execute(sql, params![row.id, stamp])?;
    }
    tx.commit()?;
    Ok(())
}

/// Settles every organisation's alert still in the queue, in one statement:
/// they are never sent (`unforwarded_alerts`), and left unmarked they would
/// sit in the queue for good. Returns how many were settled.
///
/// Every file-access line is settled here too. Since migration 20 each one
/// belongs to the organisation that owns its share — it names that tenant's
/// share, account, client address and file — and the forwarding target is
/// ONE fleet-wide setting any organisation's admin may point at its own
/// collector, so an access line is withheld exactly like an owned alert
/// until targets are per organisation (backlog). No
/// `unforwarded_access_events` exists any more for the same reason
/// `unforwarded_alerts` filters in its own statement: a query that can hand
/// out a tenant's line is one caller away from sending it.
pub fn settle_withheld_alerts(pool: &DbPool) -> Result<usize> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    let stamp = now();
    let alerts = tx.execute(
        "UPDATE nas_alerts SET forwarded_at = ?1 WHERE forwarded_at IS NULL AND org_id IS NOT NULL",
        params![stamp],
    )?;
    let access = tx.execute(
        "UPDATE nas_access_events SET forwarded_at = ?1 WHERE forwarded_at IS NULL",
        params![stamp],
    )?;
    tx.commit()?;
    Ok(alerts + access)
}

/// How many rows still wait to be SENT, so the settings card can show a
/// backlog instead of a silent stall.
///
/// An organisation's alert is never sent, so it is not pending: counting it
/// would show every tenant a backlog of other tenants' alerts that never
/// drains while forwarding is off. Counted here, in SQL, rather than by
/// loading every such id — a set that grew without bound for as long as
/// forwarding stayed off. File-access lines are never pending for the same
/// reason (each belongs to an organisation, `settle_withheld_alerts`), so the
/// access switch no longer adds anything to this figure.
pub fn forward_pending(pool: &DbPool) -> Result<u32> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nas_alerts WHERE forwarded_at IS NULL AND org_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(total.max(0) as u32)
}

// ----- shares --------------------------------------------------------------------

/// One file share as the node wants it to be. `smb`/`nfs` mirror the protocol
/// column: exactly one is `Some`, and the SMB grants come from
/// `nas_share_grants` rather than from the JSON, so deleting a user is one
/// statement instead of a rewrite of every share.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShareRow {
    pub share_id: String,
    pub name: String,
    pub protocol: String,
    pub source_path: String,
    pub dataset: Option<String>,
    pub enabled: bool,
    pub fleet_mount: bool,
    pub smb: Option<NasSmbOptions>,
    pub nfs: Option<NasNfsOptions>,
    pub state: String,
    pub state_detail: String,
    pub created_at: String,
    pub updated_at: String,
}

const SHARE_COLUMNS: &str = "share_id, name, protocol, source_path, dataset, enabled, \
                             fleet_mount, options_json, state, state_detail, created_at, updated_at";

fn share_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShareRow> {
    let protocol: String = r.get(2)?;
    let options: String = r.get(7)?;
    let (smb, nfs) = if protocol == "smb" {
        (Some(serde_json::from_str(&options).unwrap_or_default()), None)
    } else {
        (None, Some(serde_json::from_str(&options).unwrap_or_default()))
    };
    Ok(ShareRow {
        share_id: r.get(0)?,
        name: r.get(1)?,
        source_path: r.get(3)?,
        dataset: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        fleet_mount: r.get::<_, i64>(6)? != 0,
        smb,
        nfs,
        state: r.get(8)?,
        state_detail: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        protocol,
    })
}

/// The grants of one share, ordered so the generated `valid users` line is
/// stable — a config that reshuffles itself would reload smbd for nothing.
pub fn share_grants(pool: &DbPool, share_id: &str) -> Result<Vec<NasShareAccess>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT user, mode FROM nas_share_grants WHERE share_id = ?1 ORDER BY user",
    )?;
    let rows = stmt
        .query_map(params![share_id], |r| {
            Ok(NasShareAccess {
                user: r.get(0)?,
                mode: r.get(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn fill_grants(pool: &DbPool, share: &mut ShareRow) -> Result<()> {
    if let Some(smb) = share.smb.as_mut() {
        smb.users = share_grants(pool, &share.share_id)?;
    }
    Ok(())
}

/// EVERY share of the node, whoever owns it. For the node's own work only —
/// the generated smb/exports documents, the fleet mount registry, the audit
/// collector, the source-overlap checks — which must see every export this
/// node serves. Anything answering a tenant reads `list_shares_of_org`.
pub fn list_shares(pool: &DbPool) -> Result<Vec<ShareRow>> {
    let mut shares = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        let mut stmt =
            conn.prepare_cached(&format!("SELECT {SHARE_COLUMNS} FROM nas_shares ORDER BY name"))?;
        let rows = stmt
            .query_map([], share_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for share in shares.iter_mut() {
        fill_grants(pool, share)?;
    }
    Ok(shares)
}

/// The rows of one organisation (migration 20). An empty org id matches
/// nothing, never the unowned '' rows.
const OWNED_BY_SQL: &str = "org_id = ?1 AND ?1 <> ''";

/// The shares `org_id` owns — the only list a tenant is ever shown.
pub fn list_shares_of_org(pool: &DbPool, org_id: &str) -> Result<Vec<ShareRow>> {
    let mut shares = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {SHARE_COLUMNS} FROM nas_shares WHERE {OWNED_BY_SQL} ORDER BY name"
        ))?;
        let rows = stmt
            .query_map(params![org_id], share_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for share in shares.iter_mut() {
        fill_grants(pool, share)?;
    }
    Ok(shares)
}

/// Every share's owner, keyed by share id — for the node-wide checks that
/// must see every share (a source already exported, a name already taken)
/// and still may not NAME another organisation's share in their answer.
pub fn share_owners(pool: &DbPool) -> Result<std::collections::HashMap<String, String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached("SELECT share_id, org_id FROM nas_shares")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}

/// One share of `org_id`. Another organisation's share answers `None`,
/// exactly like an id that does not exist, so the answer confirms nothing.
pub fn share(pool: &DbPool, org_id: &str, share_id: &str) -> Result<Option<ShareRow>> {
    let row = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        conn.query_row(
            &format!("SELECT {SHARE_COLUMNS} FROM nas_shares WHERE {OWNED_BY_SQL} AND share_id = ?2"),
            params![org_id, share_id],
            share_from_row,
        )
        .optional()?
    };
    match row {
        Some(mut share) => {
            fill_grants(pool, &mut share)?;
            Ok(Some(share))
        }
        None => Ok(None),
    }
}

pub fn share_by_name(pool: &DbPool, name: &str) -> Result<Option<ShareRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!("SELECT {SHARE_COLUMNS} FROM nas_shares WHERE name = ?1"),
            params![name],
            share_from_row,
        )
        .optional()?)
}

fn options_json(share: &ShareRow) -> Result<String> {
    // The grants live in their own table; storing them twice would let the two
    // copies disagree the moment a user is deleted.
    Ok(match (&share.smb, &share.nfs) {
        (Some(smb), _) => serde_json::to_string(&NasSmbOptions {
            users: Vec::new(),
            ..smb.clone()
        })?,
        (_, Some(nfs)) => serde_json::to_string(nfs)?,
        _ => "{}".to_string(),
    })
}

/// Writes the share and replaces its grants in one transaction: a share whose
/// section names a user that is not in `nas_share_grants` would export access
/// nobody granted.
///
/// `org_id` is the OWNER: stamped on insert, never changed by an update, and
/// an update of a row another organisation owns writes nothing and fails —
/// the grants are only replaced after the share row itself was accepted, in
/// the same transaction, so a refused write leaves both untouched.
pub fn upsert_share(pool: &DbPool, org_id: &str, share: &ShareRow) -> Result<()> {
    anyhow::ensure!(!org_id.is_empty(), "a share needs an owning organisation");
    let options = options_json(share)?;
    let grants = share
        .smb
        .as_ref()
        .map(|s| s.users.clone())
        .unwrap_or_default();
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    let written = tx.execute(
        "INSERT INTO nas_shares (share_id, name, protocol, source_path, dataset, enabled,
                                 fleet_mount, options_json, state, state_detail, created_at,
                                 updated_at, org_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(share_id) DO UPDATE SET
            source_path = excluded.source_path, dataset = excluded.dataset,
            enabled = excluded.enabled, fleet_mount = excluded.fleet_mount,
            options_json = excluded.options_json, state = excluded.state,
            state_detail = excluded.state_detail, updated_at = excluded.updated_at
         WHERE nas_shares.org_id = excluded.org_id",
        params![
            share.share_id,
            share.name,
            share.protocol,
            share.source_path,
            share.dataset,
            i64::from(share.enabled),
            i64::from(share.fleet_mount),
            options,
            share.state,
            share.state_detail,
            share.created_at,
            share.updated_at,
            org_id
        ],
    )?;
    // Dropping the transaction rolls it back: nothing of the refused write
    // lands, and the grants below are never reached.
    anyhow::ensure!(written == 1, "share not found");
    tx.execute(
        "DELETE FROM nas_share_grants WHERE share_id = ?1",
        params![share.share_id],
    )?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO nas_share_grants (share_id, user, mode) VALUES (?1, ?2, ?3)",
        )?;
        for grant in &grants {
            stmt.execute(params![share.share_id, grant.user, grant.mode])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn set_share_state(pool: &DbPool, share_id: &str, state: &str, detail: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_shares SET state = ?2, state_detail = ?3 WHERE share_id = ?1",
        params![share_id, state, detail],
    )?;
    Ok(())
}

/// Deletes one share of `org_id` and its grants. Another organisation's
/// share is not touched and answers `false`, like a missing one.
pub fn delete_share(pool: &DbPool, org_id: &str, share_id: &str) -> Result<bool> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    let removed = tx.execute(
        &format!("DELETE FROM nas_shares WHERE {OWNED_BY_SQL} AND share_id = ?2"),
        params![org_id, share_id],
    )?;
    if removed == 1 {
        tx.execute(
            "DELETE FROM nas_share_grants WHERE share_id = ?1",
            params![share_id],
        )?;
    }
    tx.commit()?;
    Ok(removed == 1)
}

// ----- block targets (§5.5) -------------------------------------------------------

/// The parts of a target that are structure rather than scalars. Stored as one
/// JSON document because they are always read and written together, and split
/// out of `TargetRow` so the column list stays readable.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct TargetSpec {
    #[serde(default)]
    luns: Vec<NasTargetLun>,
    #[serde(default)]
    portals: Vec<NasTargetPortal>,
    #[serde(default)]
    port_groups: Vec<NasTargetPortGroup>,
}

/// One block target as the node wants it to be.
///
/// The two secret fields hold what is IN the database — `SettingsCipher`
/// ciphertext — not a plaintext secret. Nothing in this module decrypts:
/// `targets.rs` does that once, on its way to the helper's stdin.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TargetRow {
    pub target_id: String,
    pub name: String,
    pub protocol: String,
    pub wwn: String,
    pub enabled: bool,
    pub luns: Vec<NasTargetLun>,
    pub portals: Vec<NasTargetPortal>,
    pub port_groups: Vec<NasTargetPortGroup>,
    pub initiators: Vec<String>,
    pub auth_method: String,
    pub auth_username: String,
    pub auth_secret: String,
    pub auth_mutual_username: String,
    pub auth_mutual_secret: String,
    pub dhchap_hash: String,
    pub dhchap_dhgroup: String,
    pub state: String,
    pub state_detail: String,
    pub created_at: String,
    pub updated_at: String,
}

const TARGET_COLUMNS: &str = "target_id, name, protocol, wwn, enabled, spec_json, auth_method, \
                              auth_username, auth_secret, auth_mutual_username, \
                              auth_mutual_secret, dhchap_hash, dhchap_dhgroup, state, \
                              state_detail, created_at, updated_at";

fn target_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TargetRow> {
    let spec: String = r.get(5)?;
    let spec: TargetSpec = serde_json::from_str(&spec).unwrap_or_default();
    Ok(TargetRow {
        target_id: r.get(0)?,
        name: r.get(1)?,
        protocol: r.get(2)?,
        wwn: r.get(3)?,
        enabled: r.get::<_, i64>(4)? != 0,
        luns: spec.luns,
        portals: spec.portals,
        port_groups: spec.port_groups,
        initiators: Vec::new(),
        auth_method: r.get(6)?,
        auth_username: r.get(7)?,
        auth_secret: r.get(8)?,
        auth_mutual_username: r.get(9)?,
        auth_mutual_secret: r.get(10)?,
        dhchap_hash: r.get(11)?,
        dhchap_dhgroup: r.get(12)?,
        state: r.get(13)?,
        state_detail: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
    })
}

/// The allowlist of one target, ordered so the generated configfs is stable —
/// a plan that reshuffles itself would recreate ACLs for nothing.
pub fn target_initiators(pool: &DbPool, target_id: &str) -> Result<Vec<String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT initiator FROM nas_target_initiators WHERE target_id = ?1 ORDER BY initiator",
    )?;
    let rows = stmt
        .query_map(params![target_id], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// EVERY target of the node, whoever owns it. For the node's own work only —
/// the configfs reconcile, the restore after a reboot, the zvol and host-NQN
/// collision checks — which must see every object in the kernel. Anything
/// answering a tenant reads `list_targets_of_org`.
pub fn list_targets(pool: &DbPool) -> Result<Vec<TargetRow>> {
    let mut targets = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        let mut stmt = conn
            .prepare_cached(&format!("SELECT {TARGET_COLUMNS} FROM nas_targets ORDER BY name"))?;
        let rows = stmt
            .query_map([], target_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for target in targets.iter_mut() {
        target.initiators = target_initiators(pool, &target.target_id)?;
    }
    Ok(targets)
}

/// The targets `org_id` owns — the only list a tenant is ever shown.
pub fn list_targets_of_org(pool: &DbPool, org_id: &str) -> Result<Vec<TargetRow>> {
    let mut targets = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {TARGET_COLUMNS} FROM nas_targets WHERE {OWNED_BY_SQL} ORDER BY name"
        ))?;
        let rows = stmt
            .query_map(params![org_id], target_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for target in targets.iter_mut() {
        target.initiators = target_initiators(pool, &target.target_id)?;
    }
    Ok(targets)
}

/// Every target's owner, keyed by target id — see `share_owners`.
pub fn target_owners(pool: &DbPool) -> Result<std::collections::HashMap<String, String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached("SELECT target_id, org_id FROM nas_targets")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}

/// One target of `org_id`; another organisation's answers `None`.
pub fn target(pool: &DbPool, org_id: &str, target_id: &str) -> Result<Option<TargetRow>> {
    let row = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        conn.query_row(
            &format!("SELECT {TARGET_COLUMNS} FROM nas_targets WHERE {OWNED_BY_SQL} AND target_id = ?2"),
            params![org_id, target_id],
            target_from_row,
        )
        .optional()?
    };
    match row {
        Some(mut target) => {
            target.initiators = target_initiators(pool, &target.target_id)?;
            Ok(Some(target))
        }
        None => Ok(None),
    }
}

pub fn target_by_name(pool: &DbPool, name: &str) -> Result<Option<TargetRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!("SELECT {TARGET_COLUMNS} FROM nas_targets WHERE name = ?1"),
            params![name],
            target_from_row,
        )
        .optional()?)
}

/// Writes the target and replaces its allowlist in one transaction. `name`,
/// `protocol` and `wwn` are set on insert and never updated: an initiator
/// identifies the disk by the WWN, so changing it is a new target.
///
/// `org_id` is the OWNER, with the rule `upsert_share` has: stamped on
/// insert, never moved, and an update of another organisation's row writes
/// nothing — neither the row nor its allowlist — and fails.
pub fn upsert_target(pool: &DbPool, org_id: &str, target: &TargetRow) -> Result<()> {
    anyhow::ensure!(!org_id.is_empty(), "a target needs an owning organisation");
    let spec = serde_json::to_string(&TargetSpec {
        luns: target.luns.clone(),
        portals: target.portals.clone(),
        port_groups: target.port_groups.clone(),
    })?;
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    let written = tx.execute(
        "INSERT INTO nas_targets (target_id, name, protocol, wwn, enabled, spec_json,
                                  auth_method, auth_username, auth_secret,
                                  auth_mutual_username, auth_mutual_secret, dhchap_hash,
                                  dhchap_dhgroup, state, state_detail, created_at, updated_at,
                                  org_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(target_id) DO UPDATE SET
            enabled = excluded.enabled, spec_json = excluded.spec_json,
            auth_method = excluded.auth_method, auth_username = excluded.auth_username,
            auth_secret = excluded.auth_secret,
            auth_mutual_username = excluded.auth_mutual_username,
            auth_mutual_secret = excluded.auth_mutual_secret,
            dhchap_hash = excluded.dhchap_hash, dhchap_dhgroup = excluded.dhchap_dhgroup,
            state = excluded.state, state_detail = excluded.state_detail,
            updated_at = excluded.updated_at
         WHERE nas_targets.org_id = excluded.org_id",
        params![
            target.target_id,
            target.name,
            target.protocol,
            target.wwn,
            i64::from(target.enabled),
            spec,
            target.auth_method,
            target.auth_username,
            target.auth_secret,
            target.auth_mutual_username,
            target.auth_mutual_secret,
            target.dhchap_hash,
            target.dhchap_dhgroup,
            target.state,
            target.state_detail,
            target.created_at,
            target.updated_at,
            org_id
        ],
    )?;
    anyhow::ensure!(written == 1, "target not found");
    tx.execute(
        "DELETE FROM nas_target_initiators WHERE target_id = ?1",
        params![target.target_id],
    )?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO nas_target_initiators (target_id, initiator) VALUES (?1, ?2)",
        )?;
        for initiator in &target.initiators {
            stmt.execute(params![target.target_id, initiator])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn set_target_state(pool: &DbPool, target_id: &str, state: &str, detail: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_targets SET state = ?2, state_detail = ?3 WHERE target_id = ?1",
        params![target_id, state, detail],
    )?;
    Ok(())
}

/// Deletes one target of `org_id` and its allowlist; another organisation's
/// target is not touched and answers `false`.
pub fn delete_target(pool: &DbPool, org_id: &str, target_id: &str) -> Result<bool> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    let removed = tx.execute(
        &format!("DELETE FROM nas_targets WHERE {OWNED_BY_SQL} AND target_id = ?2"),
        params![org_id, target_id],
    )?;
    if removed == 1 {
        tx.execute(
            "DELETE FROM nas_target_initiators WHERE target_id = ?1",
            params![target_id],
        )?;
    }
    tx.commit()?;
    Ok(removed == 1)
}

/// Total targets of `org_id` and how many the last apply left in `error`.
pub fn target_counts(pool: &DbPool, org_id: &str) -> Result<(u32, u32)> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn.query_row(
        &format!("SELECT COUNT(*), COALESCE(SUM(state = 'error'), 0) FROM nas_targets WHERE {OWNED_BY_SQL}"),
        params![org_id],
        |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as u32)),
    )?)
}

/// Total shares of `org_id` and how many of them the last apply left in
/// `error` — what this node's fleet row shows THAT tenant
/// (`fleet::scope_local_shares`). There is deliberately no node-wide
/// variant: the published row is read by every tenant of every node, so it
/// carries no share count at all.
pub fn share_counts(pool: &DbPool, org_id: &str) -> Result<(u32, u32)> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn.query_row(
        &format!("SELECT COUNT(*), COALESCE(SUM(state = 'error'), 0) FROM nas_shares WHERE {OWNED_BY_SQL}"),
        params![org_id],
        |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as u32)),
    )?)
}

// ----- share users ---------------------------------------------------------------

/// The share accounts of `org_id` (migration 20), each with the names of
/// the shares OF THAT ORGANISATION that grant it. A share account is a node
/// account (Samba's passdb maps it to a POSIX user), so its NAME is unique on
/// the node; everything else about it — that it exists, its description,
/// where it is granted — is its organisation's business.
pub fn list_share_users(pool: &DbPool, org_id: &str) -> Result<Vec<NasShareUser>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT u.name, u.description, u.created_at,
                COALESCE(GROUP_CONCAT(s.name, char(10)), '')
         FROM nas_share_users u
         LEFT JOIN nas_share_grants g ON g.user = u.name
         LEFT JOIN nas_shares s ON s.share_id = g.share_id AND s.org_id = u.org_id
         WHERE u.org_id = ?1 AND ?1 <> ''
         GROUP BY u.name ORDER BY u.name",
    )?;
    let rows = stmt
        .query_map(params![org_id], |r| {
            let shares: String = r.get(3)?;
            Ok(NasShareUser {
                name: r.get(0)?,
                description: r.get(1)?,
                created_at: r.get(2)?,
                shares: shares.lines().map(str::to_string).collect(),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Who owns the share account `name`, if it exists at all. Three answers the
/// callers need apart: `None` (free), the caller's own org (theirs to
/// change), anything else (taken on this node by another organisation — its
/// password must never be set from here).
pub fn share_user_owner(pool: &DbPool, name: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT org_id FROM nas_share_users WHERE name = ?1",
            params![name],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// Whether `name` is a share account of `org_id`.
pub fn share_user_exists(pool: &DbPool, org_id: &str, name: &str) -> Result<bool> {
    Ok(share_user_owner(pool, name)?.is_some_and(|owner| !owner.is_empty() && owner == org_id))
}

/// Creates the account for `org_id` or updates ITS description. An account
/// another organisation owns is never touched: the conflict update carries
/// the owner in its WHERE, and a write that changed nothing is an error.
pub fn upsert_share_user(pool: &DbPool, org_id: &str, name: &str, description: &str) -> Result<()> {
    anyhow::ensure!(!org_id.is_empty(), "a share user needs an owning organisation");
    let conn = write(pool)?;
    let written = conn.execute(
        "INSERT INTO nas_share_users (name, description, created_at, org_id) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name) DO UPDATE SET description = excluded.description
         WHERE nas_share_users.org_id = excluded.org_id",
        params![name, description, now(), org_id],
    )?;
    anyhow::ensure!(written == 1, "this account name is already in use on this node");
    Ok(())
}

/// What a delete of a still-granted account is refused with. It names
/// neither the share nor its organisation: the asking tenant may not learn
/// either, only that the account is not free to go.
///
/// A REFUSAL CODE, not a sentence (M1): it reaches the share-accounts window
/// as the message of a `bad_request`, and the screen words it in the
/// reader's language (`errMessage`, format.js).
pub const SHARE_USER_IN_USE_ELSEWHERE: &str = "refusal:share_user_in_use_elsewhere";

/// `?1` = account name, `?2` = the org asking. One statement for the
/// read-only check and the in-transaction guard, so they cannot disagree.
const GRANTED_ELSEWHERE_SQL: &str = "SELECT EXISTS(SELECT 1 FROM nas_share_grants g
                         JOIN nas_shares s ON s.share_id = g.share_id
                        WHERE g.user = ?1 AND s.org_id <> ?2)";

/// Whether a share of an organisation OTHER than `org_id` still grants the
/// account `name`.
///
/// WHY it exists: migration 20 gave an account granted in two organisations
/// to the default one (the only owner it could prove nothing against), and
/// the account IS one node account — a POSIX user and a Samba passdb entry.
/// Deleting it from the default organisation would silently take the other
/// organisation's users off their own shares and remove the system account
/// they log in with. Such an account can only go once nobody else uses it.
pub fn share_user_granted_elsewhere(pool: &DbPool, org_id: &str, name: &str) -> Result<bool> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn.query_row(
        GRANTED_ELSEWHERE_SQL,
        params![name, org_id],
        |r| r.get(0),
    )?)
}

/// Removes `org_id`'s account and every grant naming it — a grant to an
/// account that no longer exists would keep appearing in the generated
/// `valid users` line. Another organisation's account answers `false`.
///
/// An account a share of ANOTHER organisation still grants is refused with
/// `SHARE_USER_IN_USE_ELSEWHERE` (`share_user_granted_elsewhere`), checked in
/// the same transaction as the delete so a grant written in between cannot
/// slip past it. The caller checks it too, BEFORE the system account is
/// removed; this is the guard that holds when a caller forgets.
pub fn delete_share_user(pool: &DbPool, org_id: &str, name: &str) -> Result<bool> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    // Ownership first: another organisation's account answers `false` like a
    // missing one, and never with the refusal below, which would confirm it.
    let ours: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM nas_share_users WHERE name = ?1 AND org_id = ?2 AND ?2 <> '')",
        params![name, org_id],
        |r| r.get(0),
    )?;
    if !ours {
        return Ok(false);
    }
    let elsewhere: bool = tx.query_row(
        GRANTED_ELSEWHERE_SQL,
        params![name, org_id],
        |r| r.get(0),
    )?;
    anyhow::ensure!(!elsewhere, SHARE_USER_IN_USE_ELSEWHERE);
    let removed = tx.execute(
        "DELETE FROM nas_share_users WHERE name = ?1 AND org_id = ?2 AND ?2 <> ''",
        params![name, org_id],
    )?;
    if removed == 1 {
        tx.execute("DELETE FROM nas_share_grants WHERE user = ?1", params![name])?;
    }
    tx.commit()?;
    Ok(removed == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tentanas_helper::elastic::ElasticDiskSpec;

    fn elastic_job(spec: &ElasticCreateSpec) -> NasJob {
        NasJob { job_id:uuid::Uuid::new_v4().to_string(),kind:"elastic_create".into(),subject:spec.name.clone(),
            status:"running".into(),started_by:"test".into(),started_at:now(),..Default::default() }
    }

    /// The journal owner of an array created before the addon was
    /// re-provisioned, and the identity it has now.
    fn previous_owner() -> ElasticOwner {
        ElasticOwner { org_id: "orgtentanas-rig11".into(), addon_id: "addontentanas".into() }
    }

    #[test]
    fn an_adopted_array_is_stored_under_this_addon_and_reads_back_as_its_own_intention() {
        let pool = pool();
        // What the helper answers with: the journal's spec, owner already
        // rewritten to the adopting instance.
        let mut spec = super::super::elastic::tests::create_spec("media");
        let adopting = ElasticOwner { org_id: "org-default".into(), addon_id: "tentanas-8dd19dc4".into() };
        spec.owner = adopting.clone();
        spec.cache = Some(ElasticDiskSpec {
            disk_id: "media-cache".into(), wwn: None, serial: Some("serial-media-cache".into()),
            bytes: 32 * 1024 * 1024 * 1024, expected_uuid: uuid::Uuid::new_v4().to_string(),
        });

        elastic_import(&pool, &spec, "admin").unwrap();

        // The row belongs to THIS addon, not to the owner the journal carried.
        let (org, addon, state, detail): (String, String, String, String) = pool.read().unwrap().query_row(
            "SELECT org_id,addon_id,state,state_detail FROM nas_elastic_arrays WHERE array_id=?1",
            params![spec.array_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((org.as_str(), addon.as_str()), ("org-default", "tentanas-8dd19dc4"));
        assert_eq!(state, "active");
        // The state detail is printed as text on the Elastic card and the
        // detail screen, so an adoption leaves it empty rather than write the
        // previous owner's ids there (MAJOR A, 2026-09-22).
        assert_eq!(detail, "", "an adopted array's state names nobody");

        // Nothing is readable without the origin operation row, so the read
        // back is what proves the adoption is a whole array and not a header.
        let row = elastic_array(&pool, &adopting, "media").unwrap().expect("adopted array reads back");
        assert_eq!(row.persisted_spec().unwrap(), &spec);
        assert_eq!(row.state, "active");
        assert_eq!(row.cache().count(), 1);
        assert!(elastic_arrays(&pool, &previous_owner()).unwrap().is_empty(), "the old owner sees nothing");

        // The disks are reserved exactly as a create would have reserved them,
        // so a later create cannot take one of them.
        let conn = pool.read().unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM nas_elastic_disks", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM nas_elastic_disk_aliases", [], |r| r.get::<_, i64>(0)).unwrap(), 8);
        assert_eq!(conn.query_row("SELECT kind FROM nas_elastic_operations", [], |r| r.get::<_, String>(0)).unwrap(), "import");
        assert_eq!(conn.query_row("SELECT status FROM nas_jobs WHERE kind='elastic_import'", [], |r| r.get::<_, String>(0)).unwrap(), "succeeded");
        assert!(conn.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
        drop(conn);

        assert_eq!(
            elastic_array_identities(&pool).unwrap(),
            vec![(spec.array_id.clone(), "media".to_string())],
            "and the scan now reports it as known"
        );
    }

    /// MAJOR A of 2026-09-22: the array's state detail (the Elastic card's
    /// reason line and the detail screen's state row) and the job log (the
    /// job row's sub-text and the job-log window) are all printed as text. An
    /// adoption writes no owner id into any of them — neither the owner it
    /// was taken from, which may be another installation's tenant, nor the
    /// adopting one. The audit of who it came from is the server log.
    #[test]
    fn an_adoption_writes_no_owner_id_into_the_array_state_or_the_job_log() {
        let pool = pool();
        let mut spec = super::super::elastic::tests::create_spec("media");
        spec.owner = ElasticOwner { org_id: "org-adopting-7f3a".into(), addon_id: "tentanas-adopting-91c2".into() };

        elastic_import(&pool, &spec, "admin").unwrap();

        let conn = pool.read().unwrap();
        let detail: String = conn.query_row(
            "SELECT state_detail FROM nas_elastic_arrays WHERE array_id=?1",
            params![spec.array_id], |r| r.get(0)).unwrap();
        let log: String = conn.query_row(
            "SELECT log FROM nas_jobs WHERE kind='elastic_import'", [], |r| r.get(0)).unwrap();
        assert_eq!(detail, "", "the state line stays empty");
        assert!(!log.is_empty(), "the job still says what happened");
        let previous = previous_owner();
        for id in [&spec.owner.org_id, &spec.owner.addon_id, &previous.org_id, &previous.addon_id] {
            assert!(!log.contains(id.as_str()), "the job log names no owner id ({id}): {log}");
        }
        assert!(!log.contains("from owner"), "nor a previous owner at all: {log}");
    }

    #[test]
    fn an_adoption_that_collides_refuses_in_words_and_writes_nothing() {
        let pool = pool();
        let created = super::super::elastic::tests::create_spec("produkt");
        insert_job(&pool, &elastic_job(&created), Some(&ElasticJobIntent::Create(created.clone()))).unwrap();

        let rows = |table: &str| pool.read().unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap();
        let (arrays, disks, aliases, jobs) = (rows("nas_elastic_arrays"), rows("nas_elastic_disks"),
            rows("nas_elastic_disk_aliases"), rows("nas_jobs"));

        // Same array, offered a second time.
        let again = elastic_import(&pool, &created, "admin").unwrap_err().to_string();
        assert!(again.contains("już zapisana w tej instancji"), "{again}");

        // A different array that wants a disk this one already holds. The
        // UNIQUE columns would refuse it as a raw SQL error; the point of the
        // pre-check is that the admin is told WHICH disk.
        let mut overlapping = super::super::elastic::tests::create_spec("archiwum");
        overlapping.data[0] = created.data[0].clone();
        let taken = elastic_import(&pool, &overlapping, "admin").unwrap_err().to_string();
        assert!(taken.contains(&created.data[0].disk_id), "the refusal names the disk: {taken}");

        // Only the filesystem UUID is shared: still a refusal, because that
        // UUID is the identity a second array must not be able to claim.
        let mut same_uuid = super::super::elastic::tests::create_spec("archiwum");
        same_uuid.data[0].expected_uuid = created.data[0].expected_uuid.clone();
        assert!(elastic_import(&pool, &same_uuid, "admin").is_err());

        // A different array whose NAME is taken.
        let mut renamed = super::super::elastic::tests::create_spec("produkt");
        renamed.owner = previous_owner();
        let name = elastic_import(&pool, &renamed, "admin").unwrap_err().to_string();
        assert!(name.contains("Nazwa macierzy jest już zajęta"), "{name}");

        assert_eq!(
            (rows("nas_elastic_arrays"), rows("nas_elastic_disks"), rows("nas_elastic_disk_aliases"), rows("nas_jobs")),
            (arrays, disks, aliases, jobs),
            "four refusals, and not one row written"
        );
    }

    #[test]
    fn elastic_reservation_conflicts_roll_back_all_rows_on_independent_connections() {
        for conflict in ["name","disk_id","wwn","serial","uuid"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("nas.db");
            let connect = || {
                let conn = Connection::open(&path).unwrap();
                conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
                conn.pragma_update(None,"foreign_keys","ON").unwrap();
                conn.pragma_update(None,"journal_mode","WAL").unwrap();
                migrate(&conn).unwrap();
                Arc::new(crate::db::Db::from_connection(conn))
            };
            let a = super::super::elastic::tests::create_spec("first");
            let mut b = super::super::elastic::tests::create_spec("second");
            match conflict {
                "name" => b.name=a.name.clone(),
                "disk_id" => b.data[0].disk_id=a.data[0].disk_id.clone(),
                "wwn" => b.data[0].wwn=a.data[0].wwn.clone(),
                "serial" => b.data[0].serial=a.data[0].serial.clone(),
                _ => b.data[0].expected_uuid=a.data[0].expected_uuid.clone(),
            }
            let pools = [connect(),connect()];
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let threads:Vec<_> = pools.into_iter().zip([a,b]).map(|(pool,spec)| {
                let barrier=barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    insert_job(&pool,&elastic_job(&spec),Some(&ElasticJobIntent::Create(spec)))
                })
            }).collect();
            assert_eq!(threads.into_iter().map(|t|t.join().unwrap().is_ok()).filter(|ok|*ok).count(),1,"{conflict}");
            let pool = connect();
            let conn = pool.read().unwrap();
            for (table,count) in [("nas_jobs",1),("nas_elastic_arrays",1),("nas_elastic_disks",2),
                ("nas_elastic_disk_aliases",6),("nas_elastic_operations",1)] {
                assert_eq!(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"),[],|r|r.get::<_,i64>(0)).unwrap(),count,"{conflict}:{table}");
            }
            assert_eq!(conn.pragma_query_value(None,"synchronous",|r|r.get::<_,i64>(0)).unwrap(),2);
            assert!(conn.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
        }
    }

    #[test]
    fn create_with_cache_persists_spec_aliases_and_rolls_back_conflict() {
        let pool = pool();
        let mut spec = super::super::elastic::tests::create_spec("cache-create");
        spec.cache = Some(ElasticDiskSpec {
            disk_id: "cache-cache-create".into(),
            wwn: None,
            serial: Some("serial-cache-create".into()),
            bytes: 32 * 1024 * 1024 * 1024,
            expected_uuid: uuid::Uuid::new_v4().to_string(),
        });
        let first = elastic_job(&spec);
        insert_job(&pool, &first, Some(&ElasticJobIntent::Create(spec.clone()))).unwrap();
        let stored = elastic_spec(&pool.read().unwrap(), &spec.owner, &spec.array_id).unwrap();
        assert_eq!(stored.cache.as_ref().map(|d| d.disk_id.as_str()), Some("cache-cache-create"));
        let claims = elastic_claims(&pool).unwrap();
        assert!(claims.iter().any(|claim| claim.disk_id == "cache-cache-create"
            && claim.serial.as_deref() == Some("serial-cache-create")));
        assert_eq!(
            pool.read().unwrap().query_row(
                "SELECT COUNT(*) FROM nas_elastic_disk_aliases WHERE array_id=?1 AND role='cache'",
                params![spec.array_id],
                |r| r.get::<_, i64>(0),
            ).unwrap(),
            2
        );

        let mut conflicting = super::super::elastic::tests::create_spec("cache-conflict");
        conflicting.cache = spec.cache.clone();
        let second = elastic_job(&conflicting);
        assert!(insert_job(&pool, &second, Some(&ElasticJobIntent::Create(conflicting))).is_err());
        assert_eq!(pool.read().unwrap().query_row("SELECT COUNT(*) FROM nas_jobs", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(pool.read().unwrap().query_row("SELECT COUNT(*) FROM nas_elastic_arrays", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(pool.read().unwrap().query_row("SELECT COUNT(*) FROM nas_elastic_disks", [], |r| r.get::<_, i64>(0)).unwrap(), 3);

        let mut second_valid = super::super::elastic::tests::create_spec("cache-second");
        second_valid.cache = Some(ElasticDiskSpec {
            disk_id: "cache-cache-second".into(), wwn: None,
            serial: Some("serial-cache-second".into()), bytes: 32 * 1024 * 1024 * 1024,
            expected_uuid: uuid::Uuid::new_v4().to_string(),
        });
        let second_job = elastic_job(&second_valid);
        insert_job(&pool, &second_job, Some(&ElasticJobIntent::Create(second_valid.clone()))).unwrap();
        assert_eq!(pool.read().unwrap().query_row("SELECT COUNT(*) FROM nas_jobs WHERE status='failed'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(pool.read().unwrap().query_row("SELECT COUNT(*) FROM nas_elastic_arrays", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }

    #[test]
    fn elastic_wwn_only_survives_orphan_without_releasing_uuid() {
        let p = pool();
        let mut spec=super::super::elastic::tests::create_spec("wwnonly");
        spec.data[0].serial=None;
        let job=elastic_job(&spec);
        insert_job(&p,&job,Some(&ElasticJobIntent::Create(spec.clone()))).unwrap();
        assert_eq!(fail_orphaned_jobs(&p).unwrap(),1);
        let row=elastic_array(&p,&spec.owner,&spec.name).unwrap().unwrap();
        assert_eq!(row.persisted_spec().unwrap(),&spec);
        assert_eq!(row.state,"needs_attention");
        let foreign=ElasticOwner {org_id:"foreign".into(),addon_id:spec.owner.addon_id.clone()};
        assert!(elastic_arrays(&p,&foreign).unwrap().is_empty());
        let conn=p.write().unwrap();
        conn.execute("UPDATE nas_elastic_disks SET expected_uuid=?1 WHERE role='data'",
            params![uuid::Uuid::new_v4().to_string()]).unwrap();
        drop(conn);
        assert!(elastic_arrays(&p,&spec.owner).is_err());
    }

    #[test]
    fn elastic_schema_upgrade_preserves_old_job_and_reopens_exact_spec() {
        let dir=tempfile::tempdir().unwrap();
        let path=dir.path().join("upgrade.db");
        let conn=Connection::open(&path).unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn,APP,&MIGRATIONS[..8]).unwrap();
        conn.execute("INSERT INTO nas_jobs(job_id,kind,subject,status,started_by,started_at)
            VALUES ('old','scrub','tank','succeeded','admin','2026-09-07')",[]).unwrap();
        migrate(&conn).unwrap();
        let p=Arc::new(crate::db::Db::from_connection(conn));
        let spec=super::super::elastic::tests::create_spec("reopen");
        insert_job(&p,&elastic_job(&spec),Some(&ElasticJobIntent::Create(spec.clone()))).unwrap();
        drop(p);
        let conn=Connection::open(&path).unwrap();
        migrate(&conn).unwrap();
        let p=Arc::new(crate::db::Db::from_connection(conn));
        assert_eq!(job(&p,"old").unwrap().unwrap().subject,"tank");
        assert_eq!(elastic_array(&p,&spec.owner,&spec.name).unwrap().unwrap().persisted_spec().unwrap(),&spec);
    }

    #[test]
    fn elastic_restore_is_owner_scoped_atomic_and_keeps_original_create_id() {
        let p=pool();
        let spec=super::super::elastic::tests::create_spec("restore");
        insert_job(&p,&elastic_job(&spec),Some(&ElasticJobIntent::Create(spec.clone()))).unwrap();
        let mut restore=elastic_job(&spec);
        restore.kind="elastic_restore".into();
        let operation_id=uuid::Uuid::new_v4().to_string();
        let intent=ElasticJobIntent::Restore {owner:spec.owner.clone(),array_id:spec.array_id.clone(),operation_id:operation_id.clone()};
        assert!(insert_job(&p,&restore,Some(&intent)).is_err());
        assert_eq!(list_jobs(&p,100).unwrap().len(),1);
        finish_elastic_operation(&p,&spec.owner,&spec.operation_id,
            Ok(&super::super::elastic::tests::ready_result(&spec))).unwrap();
        let foreign=ElasticJobIntent::Restore {owner:ElasticOwner {org_id:"foreign".into(),addon_id:"nas".into()},
            array_id:spec.array_id.clone(),operation_id:uuid::Uuid::new_v4().to_string()};
        assert!(insert_job(&p,&restore,Some(&foreign)).is_err());
        insert_job(&p,&restore,Some(&intent)).unwrap();
        finish_elastic_operation(&p,&spec.owner,&operation_id,
            Ok(&super::super::elastic::tests::ready_result(&spec))).unwrap();
        assert_eq!(elastic_array(&p,&spec.owner,&spec.name).unwrap().unwrap().persisted_spec().unwrap(),&spec);
        assert_eq!(list_jobs(&p,100).unwrap().len(),2);
    }

    fn completed_array(pool: &DbPool, name: &str) -> ElasticCreateSpec {
        let spec = super::super::elastic::tests::create_spec(name);
        let created = elastic_job(&spec);
        insert_job(
            pool,
            &created,
            Some(&ElasticJobIntent::Create(spec.clone())),
        )
        .unwrap();
        finish_elastic_operation(
            pool,
            &spec.owner,
            &spec.operation_id,
            Ok(&super::super::elastic::tests::ready_result(&spec)),
        )
        .unwrap();
        finish_job(pool, &created.job_id, "succeeded", None).unwrap();
        spec
    }

    fn maintenance(
        spec: &ElasticCreateSpec,
        kind: tentanas_helper::elastic::ElasticSnapraidKind,
    ) -> (NasJob, ElasticJobIntent, String) {
        let mut row = elastic_job(spec);
        row.kind = format!("elastic_{}", super::super::elastic::snapraid_kind(&kind));
        let id = uuid::Uuid::now_v7().to_string();
        let intent = ElasticJobIntent::Snapraid {
            owner: spec.owner.clone(),
            array_id: spec.array_id.clone(),
            operation_id: id.clone(),
            kind,
            acknowledge_parity_fault: None,
        };
        (row, intent, id)
    }

    #[test]
    fn schema_ten_preserves_schema_nine_operation_and_rejects_a_second_running_job() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("maintenance-race.db");
        let conn = Connection::open(&path).unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..9]).unwrap();
        let p = Arc::new(crate::db::Db::from_connection(conn));
        let spec = super::super::elastic::tests::create_spec("schema-ten");
        let created = elastic_job(&spec);
        // Written the way the SCHEMA-9 BUILD wrote it, by hand: `insert_job` is
        // today's code and writes today's columns (`org_id`, migration 18), which
        // a schema-9 table does not have. Using it here would test that the
        // current writer can fill an old table — which it never has to, because
        // the database is always migrated before the app writes — instead of
        // what this test is about: a row from back then surviving migration 10.
        {
            let conn = p.write().unwrap();
            conn.execute(
                "INSERT INTO nas_elastic_arrays
                 (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,'creating','',?6,?6)",
                params![spec.array_id, spec.owner.org_id, spec.owner.addon_id, spec.name,
                    spec.filesystem.as_str(), created.started_at],
            ).unwrap();
            conn.execute(
                "INSERT INTO nas_jobs (job_id, kind, subject, status, progress_pct, started_by,
                                       started_at, log)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '')",
                params![created.job_id, created.kind, created.subject, created.status,
                    created.progress_pct.map(i64::from), created.started_by, created.started_at],
            ).unwrap();
            insert_elastic_disks(&conn, &spec).unwrap();
            conn.execute(
                "INSERT INTO nas_elastic_operations
                 (operation_id, array_id, job_id, kind, state, request_json, error, created_at)
                 VALUES (?1, ?2, ?3, 'create', 'running', ?4, '', ?5)",
                params![spec.operation_id, spec.array_id, created.job_id,
                    serde_json::to_string(&spec).unwrap(), created.started_at],
            ).unwrap();
        }
        let before: String = p
            .read()
            .unwrap()
            .query_row("SELECT request_json FROM nas_elastic_operations", [], |r| {
                r.get(0)
            })
            .unwrap();
        migrate(&p.write().unwrap()).unwrap();
        let after: String = p
            .read()
            .unwrap()
            .query_row("SELECT request_json FROM nas_elastic_operations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(before, after);
        finish_elastic_operation(
            &p,
            &spec.owner,
            &spec.operation_id,
            Ok(&super::super::elastic::tests::ready_result(&spec)),
        )
        .unwrap();
        finish_job(&p, &created.job_id, "succeeded", None).unwrap();
        drop(p);
        let open = || {
            let conn = Connection::open(&path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            migrate(&conn).unwrap();
            Arc::new(crate::db::Db::from_connection(conn))
        };
        let pools = [open(), open()];
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = pools
            .into_iter()
            .zip([
                tentanas_helper::elastic::ElasticSnapraidKind::Sync,
                tentanas_helper::elastic::ElasticSnapraidKind::Scrub,
            ])
            .map(|(p, kind)| {
                let spec = spec.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let (job, intent, _) = maintenance(&spec, kind);
                    barrier.wait();
                    insert_job(&p, &job, Some(&intent)).is_ok()
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .filter(|ok| *ok)
                .count(),
            1
        );
        let p = open();
        assert_eq!(list_jobs(&p, 100).unwrap().len(), 2);
        let conn = p.read().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM nas_elastic_operations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert!(
            conn.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn schema_eleven_adds_cache_role_and_keeps_alias_foreign_keys() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        assert_eq!(conn.pragma_query_value(None, "foreign_keys", |r| r.get::<_, i64>(0)).unwrap(), 1);
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..10]).unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_arrays
             (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
             VALUES ('11111111-1111-4111-8111-111111111111','org','addon','cache-test','xfs','active','','now','now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_disks
             (array_id,role,slot,disk_id,wwn,serial,bytes,expected_uuid)
             VALUES ('11111111-1111-4111-8111-111111111111','data',1,'data-1','wwn-1','serial-1',100,'22222222-2222-4222-8222-222222222222')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_disk_aliases
             (kind,value,array_id,role,slot)
             VALUES ('disk_id','data-1','11111111-1111-4111-8111-111111111111','data',1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_disk_aliases
             (kind,value,array_id,role,slot)
             VALUES ('wwn','wwn-1','11111111-1111-4111-8111-111111111111','data',1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_disk_aliases
             (kind,value,array_id,role,slot)
             VALUES ('serial','serial-1','11111111-1111-4111-8111-111111111111','data',1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_disks
             (array_id,role,slot,disk_id,wwn,serial,bytes,expected_uuid)
             VALUES ('11111111-1111-4111-8111-111111111111','parity',1,'parity-1','wwn-parity','serial-parity',100,'66666666-6666-4666-8666-666666666666')",
            [],
        ).unwrap();
        for (kind, value) in [("disk_id", "parity-1"), ("wwn", "wwn-parity"), ("serial", "serial-parity")] {
            conn.execute(
                "INSERT INTO nas_elastic_disk_aliases (kind,value,array_id,role,slot) VALUES (?1,?2,'11111111-1111-4111-8111-111111111111','parity',1)",
                rusqlite::params![kind, value],
            ).unwrap();
        }
        let old_aliases: Vec<(String,String,String,i64)> = conn.prepare(
            "SELECT kind,value,role,slot FROM nas_elastic_disk_aliases WHERE array_id='11111111-1111-4111-8111-111111111111' ORDER BY kind,value"
        ).unwrap().query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))
            .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_disks
             (array_id,role,slot,disk_id,wwn,serial,bytes,expected_uuid)
             VALUES ('11111111-1111-4111-8111-111111111111','cache',1,'cache-1',NULL,'serial-cache',100,'33333333-3333-4333-8333-333333333333')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_disk_aliases
             (kind,value,array_id,role,slot)
             VALUES ('serial','serial-cache','11111111-1111-4111-8111-111111111111','cache',1)",
            [],
        )
        .unwrap();
        assert!(conn.execute(
            "INSERT INTO nas_elastic_disks
             (array_id,role,slot,disk_id,wwn,serial,bytes,expected_uuid)
             VALUES ('11111111-1111-4111-8111-111111111111','cache',2,'cache-2',NULL,'serial-cache-2',100,'44444444-4444-4444-8444-444444444444')",
            [],
        ).is_err());
        assert!(conn.execute(
            "INSERT INTO nas_elastic_disks
             (array_id,role,slot,disk_id,wwn,serial,bytes,expected_uuid)
             VALUES ('11111111-1111-4111-8111-111111111111','data',2,'data-1','wwn-2','serial-2',100,'55555555-5555-4555-8555-555555555555')",
            [],
        ).is_err());
        assert!(conn.execute(
            "INSERT INTO nas_elastic_disk_aliases
             (kind,value,array_id,role,slot)
             VALUES ('serial','serial-cache','11111111-1111-4111-8111-111111111111','cache',1)",
            [],
        ).is_err());
        let role: String = conn
            .query_row("SELECT role FROM nas_elastic_disks WHERE disk_id='cache-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(role, "cache");
        let old_alias_count = old_aliases.len() as i64;
        for (kind, value, role, slot) in old_aliases {
            assert_eq!(conn.query_row(
                "SELECT COUNT(*) FROM nas_elastic_disk_aliases WHERE kind=?1 AND value=?2 AND role=?3 AND slot=?4",
                rusqlite::params![kind, value, role, slot], |r| r.get::<_, i64>(0),
            ).unwrap(), 1);
        }
        let cache_alias_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM nas_elastic_disk_aliases WHERE array_id='11111111-1111-4111-8111-111111111111' AND kind='serial' AND value='serial-cache' AND role='cache' AND slot=1",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(cache_alias_count, 1);
        assert_eq!(conn.query_row(
            "SELECT COUNT(*) FROM nas_elastic_disk_aliases WHERE array_id='11111111-1111-4111-8111-111111111111'",
            [], |r| r.get::<_, i64>(0),
        ).unwrap(), old_alias_count + 1);
        assert!(
            conn.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn schema_twelve_widens_the_operation_kind_and_keeps_every_row_and_index() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..11]).unwrap();
        let array_id = "11111111-1111-4111-8111-111111111111";
        conn.execute(
            "INSERT INTO nas_elastic_arrays
             (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
             VALUES (?1,'org','addon','legacy','xfs','active','','now','now')",
            params![array_id],
        )
        .unwrap();
        // A v11 database with real history, including a row closed as
        // needs_attention — the state a rebuild would be most likely to drop.
        for (job, kind, state) in [
            ("job-create", "create", "succeeded"),
            ("job-sync", "sync", "needs_attention"),
            ("job-scrub", "scrub", "succeeded"),
        ] {
            conn.execute(
                "INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at,log)
                 VALUES (?1,?2,'legacy','succeeded','test','now','')",
                params![job, format!("elastic_{kind}")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO nas_elastic_operations
                 (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
                 VALUES (?1,?2,?3,?4,?5,'{}','','2026-09-01T00:00:00Z')",
                params![format!("op-{kind}"), array_id, job, kind, state],
            )
            .unwrap();
        }
        let before: Vec<(String, String, String)> = conn
            .prepare("SELECT operation_id,kind,state FROM nas_elastic_operations ORDER BY operation_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let indexes = |conn: &Connection| -> Vec<String> {
            conn.prepare(
                "SELECT name FROM sqlite_master WHERE type='index'
                 AND tbl_name='nas_elastic_operations' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
        };
        let indexes_before = indexes(&conn);

        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();

        let after: Vec<(String, String, String)> = conn
            .prepare("SELECT operation_id,kind,state FROM nas_elastic_operations ORDER BY operation_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(after, before, "rebuild zgubił lub zmienił wiersz");
        // Migration 14 rebuilds this table once more and renames the create
        // guard: an array's ORIGIN operation is now either the create that
        // made it or the import that adopted it, and exactly one of the two
        // may exist. Migration 15 rebuilds it a third time for the three
        // lifecycle kinds and adds no index of its own. Every index survives
        // all three rebuilds.
        let renamed: Vec<String> = indexes_before
            .iter()
            .map(|name| match name.as_str() {
                "nas_elastic_operation_create" => "nas_elastic_operation_origin".to_string(),
                other => other.to_string(),
            })
            .collect();
        assert_eq!(indexes(&conn), renamed, "rebuild zgubił indeks");
        assert!(
            conn.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
        // The point of the rebuild: 'mover' is now a legal kind, and the unique
        // running index still refuses a second running operation per array.
        conn.execute(
            "INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at,log)
             VALUES ('job-mover','elastic_mover','legacy','running','test','now','')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_operations
             (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
             VALUES ('op-mover',?1,'job-mover','mover','running','{}','','2026-09-02T00:00:00Z')",
            params![array_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at,log)
             VALUES ('job-mover-2','elastic_mover','legacy','running','test','now','')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO nas_elastic_operations
                 (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
                 VALUES ('op-mover-2',?1,'job-mover-2','mover','running','{}','','2026-09-03T00:00:00Z')",
                params![array_id],
            )
            .is_err(),
            "druga running operacja musi pozostać niemożliwa"
        );
        // An unknown kind is still refused: the CHECK was widened, not dropped.
        assert!(
            conn.execute(
                "INSERT INTO nas_elastic_operations
                 (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
                 VALUES ('op-bogus',?1,'job-mover-2','wat','succeeded','{}','','2026-09-04T00:00:00Z')",
                params![array_id],
            )
            .is_err()
        );
        // Migration 14: 'import' is a legal kind, and the origin index allows
        // ONE origin per array — this one already has its create, so the
        // adoption row is refused rather than leaving two answers to "where
        // did this array come from".
        conn.execute(
            "INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at,log)
             VALUES ('job-import','elastic_import','legacy','succeeded','test','now','')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO nas_elastic_operations
                 (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
                 VALUES ('op-import',?1,'job-import','import','succeeded','{}','','2026-09-04T00:00:00Z')",
                params![array_id],
            )
            .is_err(),
            "macierz z operacją create nie może dostać drugiej operacji pochodzenia"
        );

        // schema13 against the SAME seeded database: an UPGRADED instance, not
        // just a freshly created one, really has somewhere to put a cadence.
        conn.execute(
            r#"INSERT INTO nas_elastic_schedules (array_id,kind,enabled,schedule_json,next_run_at)
               VALUES (?1,'mover',1,'{"every":"1h","hour":0,"minute":0,"weekday":0,"day":1}','2026-09-05T00:00:00Z')"#,
            params![array_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_mover_settings (array_id,min_age_secs,cache_min_free_pct,coupled_sync)
             VALUES (?1,1800,35,0)",
            params![array_id],
        )
        .unwrap();
        // One row per (array, kind): the primary key refuses a second.
        assert!(
            conn.execute(
                r#"INSERT INTO nas_elastic_schedules (array_id,kind,enabled,schedule_json)
                   VALUES (?1,'mover',0,'{}')"#,
                params![array_id],
            )
            .is_err(),
            "duplikat (array_id, kind) musi zostać odrzucony"
        );
        // Exactly three kinds: a fourth is refused by the CHECK.
        assert!(
            conn.execute(
                r#"INSERT INTO nas_elastic_schedules (array_id,kind,enabled,schedule_json)
                   VALUES (?1,'trim',1,'{}')"#,
                params![array_id],
            )
            .is_err(),
            "czwarty rodzaj harmonogramu musi zostać odrzucony"
        );
        // And the foreign keys really point at an array that exists.
        assert!(
            conn.execute(
                r#"INSERT INTO nas_elastic_schedules (array_id,kind,enabled,schedule_json)
                   VALUES ('no-such-array','sync',1,'{}')"#,
                [],
            )
            .is_err(),
            "FK musi odrzucić nieistniejącą macierz"
        );
        assert!(
            conn.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none(),
            "nowe tabele nie mogą zostawić wiszącego klucza obcego"
        );
    }

    fn mover_intent(spec: &ElasticCreateSpec, operation_id: &str, resume: &str) -> ElasticJobIntent {
        ElasticJobIntent::Mover {
            owner: spec.owner.clone(),
            array_id: spec.array_id.clone(),
            operation_id: operation_id.to_string(),
            resume_operation_id: resume.to_string(),
            rules: tentanas_helper::elastic::MoverRules {
                min_age_secs: 7200,
                min_free_pct: 20,
                pinned_folders: Vec::new(),
                eager_folders: Vec::new(),
                skip_open_files: true,
            },
            coupled_sync: true,
        }
    }

    fn mover_job(spec: &ElasticCreateSpec) -> NasJob {
        let mut row = elastic_job(spec);
        row.kind = "elastic_mover".into();
        row
    }

    #[test]
    fn a_mover_intent_needs_its_own_resume_a_matching_job_kind_and_a_settled_array() {
        let p = pool();
        let spec = completed_array(&p, "mover-intent");
        let id = uuid::Uuid::now_v7().to_string();
        // The resume is a SEPARATE reservation: reusing the run's own operation
        // or Create's produces a command the helper refuses outright.
        assert!(insert_job(&p, &mover_job(&spec), Some(&mover_intent(&spec, &id, &id))).is_err());
        assert!(
            insert_job(&p, &mover_job(&spec), Some(&mover_intent(&spec, &id, &spec.operation_id)))
                .is_err()
        );
        let resume = uuid::Uuid::now_v7().to_string();
        assert!(
            insert_job(&p, &mover_job(&spec), Some(&mover_intent(&spec, &spec.operation_id, &resume)))
                .is_err()
        );
        // A mover intent may not ride on a job of another kind.
        let mut wrong_kind = elastic_job(&spec);
        wrong_kind.kind = "elastic_sync".into();
        assert!(insert_job(&p, &wrong_kind, Some(&mover_intent(&spec, &id, &resume))).is_err());
        // Nothing above wrote a row.
        assert_eq!(list_jobs(&p, 100).unwrap().len(), 1);

        let row = mover_job(&spec);
        insert_job(&p, &row, Some(&mover_intent(&spec, &id, &resume))).unwrap();
        let (kind, state): (String, String) = p
            .read()
            .unwrap()
            .query_row(
                "SELECT kind,state FROM nas_elastic_operations WHERE operation_id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((kind.as_str(), state.as_str()), ("mover", "running"));
    }

    /// Records one completed mover run that moved `bytes` and left
    /// `skipped_files` behind, and returns its id. The skip count is what
    /// decides `partial` versus `ok`, so it is a parameter rather than a
    /// constant of the fixture.
    fn recorded_mover_run(
        p: &DbPool,
        spec: &ElasticCreateSpec,
        bytes: u64,
        created_at: &str,
        skipped_files: u64,
    ) -> String {
        let id = uuid::Uuid::now_v7().to_string();
        let resume = uuid::Uuid::now_v7().to_string();
        let row = mover_job(spec);
        insert_job(p, &row, Some(&mover_intent(spec, &id, &resume))).unwrap();
        let mut result = super::super::elastic::tests::mover_result(
            spec,
            &id,
            &resume,
            tentanas_helper::elastic::ElasticMoverPhase::Complete,
            true,
        );
        result.run.moved_bytes = bytes;
        result.run.skipped_files = skipped_files;
        result.run.skipped_bytes = if skipped_files > 0 { 64 } else { 0 };
        result.state.last_mover = Some(result.run.clone());
        record_mover_result(p, &spec.owner, &id, &result).unwrap();
        finish_job(p, &row.job_id, "succeeded", None).unwrap();
        // `now()` has second resolution, so the ordering the strip depends on is
        // pinned explicitly rather than left to two writes in the same second.
        p.write()
            .unwrap()
            .execute(
                "UPDATE nas_elastic_operations SET created_at=?2 WHERE operation_id=?1",
                params![id, created_at],
            )
            .unwrap();
        id
    }

    #[test]
    fn the_mover_strip_shows_recorded_runs_newest_first_and_never_contradicts_the_last_run() {
        let p = pool();
        let spec = completed_array(&p, "mover-strip");
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert!(array.mover_history.is_empty(), "nic nie zapisano — nic nie pokazujemy");

        // The older run left two files behind; the newer one left nothing.
        recorded_mover_run(&p, &spec, 12 * 1024 * 1024 * 1024, "2026-09-01T00:00:00Z", 2);
        recorded_mover_run(&p, &spec, 42 * 1024 * 1024 * 1024, "2026-09-02T00:00:00Z", 0);
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        // The defect this guards: a populated last run beside a strip that
        // asserts no run ever happened.
        assert_eq!(array.mover_history.len(), 2);
        assert_eq!(array.mover_history[0].moved_bytes, 42 * 1024 * 1024 * 1024);
        assert_eq!(array.mover_history[1].moved_bytes, 12 * 1024 * 1024 * 1024);
        // BOTH arms of the distinction, pinned: `ok` is reserved for a run that
        // left nothing behind, and a run that skipped files is `partial`. A
        // mapping that answered `ok` for the second would be calling an
        // incomplete run clean — the exact lie this feature exists to prevent.
        assert_eq!(array.mover_history[0].outcome, "ok");
        assert_eq!(array.mover_history[1].outcome, "partial");
        assert_eq!(array.mover_history[1].skipped_files, 2);
        assert_eq!(array.mover_history[0].skipped_files, 0);
        assert!(array.mover_history.iter().all(|r| r.counts_known));
        // A mover run is still not a SnapRAID run.
        assert!(array.snapraid_history.is_empty());
        assert!(!array.unresolved_operation);
    }

    #[test]
    fn a_mover_row_without_a_trustworthy_result_is_skipped_rather_than_shown_as_zero() {
        let p = pool();
        let spec = completed_array(&p, "mover-untrusted");
        let id = uuid::Uuid::now_v7().to_string();
        let resume = uuid::Uuid::now_v7().to_string();
        let row = mover_job(&spec);
        insert_job(&p, &row, Some(&mover_intent(&spec, &id, &resume))).unwrap();
        let result = super::super::elastic::tests::mover_result(
            &spec,
            &id,
            &resume,
            tentanas_helper::elastic::ElasticMoverPhase::Complete,
            true,
        );
        record_mover_result(&p, &spec.owner, &id, &result).unwrap();
        // The stored result stops being trustworthy — the state `finish_job`
        // closes as needs_attention when validation fails.
        p.write()
            .unwrap()
            .execute(
                "UPDATE nas_elastic_operations SET result_json='{\"state\":1}' WHERE operation_id=?1",
                params![id],
            )
            .unwrap();
        finish_job(&p, &row.job_id, "failed", Some("uszkodzony wynik")).unwrap();
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        // No invented "0 B" run on the strip...
        assert!(array.mover_history.is_empty());
        // ...and the array says the true thing instead.
        assert_eq!(array.state, "needs_attention");
        assert!(array.unresolved_operation);
    }

    #[test]
    fn an_unresolved_operation_outlives_the_array_returning_to_active() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        let p = pool();
        let spec = completed_array(&p, "mover-stale");
        let (row, intent, id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &row, Some(&intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &id,
            &super::super::elastic::tests::snapraid_result(&spec, &id, Kind::Sync, Outcome::Failed),
        )
        .unwrap();
        finish_job(&p, &row.job_id, "failed", Some("data_error")).unwrap();
        let blocked = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(blocked.state, "needs_attention");
        assert!(blocked.unresolved_operation);

        // A successful Restore writes state='active' on the ARRAY and touches
        // only its own operation row — so the stale one survives, and the state
        // alone stops being enough to tell whether maintenance may run.
        p.write()
            .unwrap()
            .execute(
                "UPDATE nas_elastic_arrays SET state='active' WHERE array_id=?1",
                params![spec.array_id],
            )
            .unwrap();
        let restored = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(restored.state, "active");
        assert!(
            restored.unresolved_operation,
            "nierozwiązana operacja przeżywa powrót macierzy do active"
        );
    }

    #[test]
    fn an_unresolved_sync_blocks_a_new_mover() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        let p = pool();
        let spec = completed_array(&p, "mover-blocked");
        let (row, intent, id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &row, Some(&intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &id,
            &super::super::elastic::tests::snapraid_result(&spec, &id, Kind::Sync, Outcome::Failed),
        )
        .unwrap();
        finish_job(&p, &row.job_id, "failed", Some("data_error")).unwrap();
        // The sync is unresolved: nothing can say what parity currently covers,
        // so the mover must not move more bytes on top of that unknown.
        let mover = uuid::Uuid::now_v7().to_string();
        let resume = uuid::Uuid::now_v7().to_string();
        assert!(
            insert_job(&p, &mover_job(&spec), Some(&mover_intent(&spec, &mover, &resume))).is_err()
        );
    }

    #[test]
    fn a_mover_result_is_recorded_once_and_is_invisible_to_every_snapraid_query() {
        let p = pool();
        let spec = completed_array(&p, "mover-history");
        let id = uuid::Uuid::now_v7().to_string();
        let resume = uuid::Uuid::now_v7().to_string();
        let row = mover_job(&spec);
        insert_job(&p, &row, Some(&mover_intent(&spec, &id, &resume))).unwrap();
        let result = super::super::elastic::tests::mover_result(
            &spec,
            &id,
            &resume,
            tentanas_helper::elastic::ElasticMoverPhase::Complete,
            true,
        );
        record_mover_result(&p, &spec.owner, &id, &result).unwrap();
        assert!(
            record_mover_result(&p, &spec.owner, &id, &result).is_err(),
            "wynik operacji zapisuje się dokładnie raz"
        );
        finish_job(&p, &row.job_id, "succeeded", None).unwrap();
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(array.state, "active");
        // The mover row is not a SnapRAID run anywhere it could be mistaken for one.
        assert!(array.snapraid_history.is_empty());
        assert!(array.parity_window_runs.is_empty());
        assert!(array.last_sync_run.is_none());
        assert!(array.last_scrub_run.is_none());
        let state: String = p
            .read()
            .unwrap()
            .query_row(
                "SELECT state FROM nas_elastic_operations WHERE kind='mover'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "succeeded");
    }

    #[test]
    fn snapraid_finalization_is_atomic_for_abort_and_ignored_job_write() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        for (table, mode) in [
            ("nas_jobs", "ABORT"),
            ("nas_jobs", "IGNORE"),
            ("nas_elastic_operations", "ABORT"),
            ("nas_elastic_arrays", "ABORT"),
        ] {
            let p = pool();
            let spec = completed_array(&p, "atomic");
            let (row, intent, id) = maintenance(&spec, Kind::Sync);
            insert_job(&p, &row, Some(&intent)).unwrap();
            let result = super::super::elastic::tests::snapraid_result(
                &spec,
                &id,
                Kind::Sync,
                Outcome::Succeeded,
            );
            record_snapraid_result(&p, &spec.owner, &id, &result).unwrap();
            let trigger = if mode == "IGNORE" {
                "SELECT RAISE(IGNORE)"
            } else {
                "SELECT RAISE(ABORT,'test odmowy')"
            };
            p.write()
                .unwrap()
                .execute_batch(&format!(
                    "CREATE TRIGGER stop_final BEFORE UPDATE ON {table} BEGIN {trigger}; END;"
                ))
                .unwrap();
            assert!(
                finish_job(&p, &row.job_id, "succeeded", None).is_err(),
                "{table}/{mode}"
            );
            assert_eq!(job(&p, &row.job_id).unwrap().unwrap().status, "running");
            let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
            assert_eq!(array.state, "active");
            assert_eq!(array.snapraid_history[0].outcome, "running");
            assert!(array.snapraid_history[0].errors.is_none());
        }
    }

    #[test]
    fn snapraid_candidate_is_not_terminal_history_and_survives_reopen() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapraid.db");
        let open = || {
            let conn = Connection::open(&path).unwrap();
            migrate(&conn).unwrap();
            Arc::new(crate::db::Db::from_connection(conn))
        };
        let p = open();
        let spec = completed_array(&p, "history");
        let (row, intent, id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &row, Some(&intent)).unwrap();
        let result = super::super::elastic::tests::snapraid_result(
            &spec,
            &id,
            Kind::Sync,
            Outcome::Succeeded,
        );
        record_snapraid_result(&p, &spec.owner, &id, &result).unwrap();
        assert!(record_snapraid_result(&p, &spec.owner, &id, &result).is_err());
        let before = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(before.snapraid_history[0].outcome, "running");
        assert!(before.last_sync_run.is_none());
        finish_job(&p, &row.job_id, "succeeded", None).unwrap();
        drop(p);
        let p = open();
        let after = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(job(&p, &row.job_id).unwrap().unwrap().status, "succeeded");
        assert_eq!(after.snapraid_history[0].outcome, "ok");
        assert_eq!(
            after.last_sync_run.unwrap().operation_id.as_deref(),
            Some(id.as_str())
        );
        assert!(
            elastic_arrays(
                &p,
                &ElasticOwner {
                    org_id: "foreign".into(),
                    addon_id: spec.owner.addon_id.clone()
                }
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn snapraid_missing_or_foreign_candidate_never_commits_success() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        for foreign in [false, true] {
            let p = pool();
            let spec = completed_array(&p, "candidate");
            let (row, intent, id) = maintenance(&spec, Kind::Sync);
            insert_job(&p, &row, Some(&intent)).unwrap();
            if foreign {
                let mut result = super::super::elastic::tests::snapraid_result(
                    &spec,
                    &id,
                    Kind::Sync,
                    Outcome::Succeeded,
                );
                result.run.operation_id = uuid::Uuid::new_v4().to_string();
                assert!(record_snapraid_result(&p, &spec.owner, &id, &result).is_err());
                p.write()
                    .unwrap()
                    .execute(
                        "UPDATE nas_elastic_operations SET result_json=?1 WHERE operation_id=?2",
                        params![serde_json::to_string(&result).unwrap(), id],
                    )
                    .unwrap();
            }
            assert!(finish_job(&p, &row.job_id, "succeeded", None).is_err());
            assert_eq!(job(&p, &row.job_id).unwrap().unwrap().status, "failed");
            let after = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
            // No result at all reads as interrupted and leaves the array as it
            // was (W-D); a foreign one is a contradiction that needs attention.
            assert_eq!(after.state, if foreign { "needs_attention" } else { "active" });
            assert_eq!(after.snapraid_history[0].outcome, if foreign { "needs_attention" } else { INTERRUPTED_OUTCOME });
            assert!(after.last_sync_run.is_none());
        }
    }

    #[test]
    fn twenty_one_refusals_keep_last_successful_scrub_and_allow_explicit_sync() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        let p = pool();
        let spec = completed_array(&p, "refusals");
        let (row, intent, id) = maintenance(&spec, Kind::Scrub);
        insert_job(&p, &row, Some(&intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &id,
            &super::super::elastic::tests::snapraid_result(
                &spec,
                &id,
                Kind::Scrub,
                Outcome::Succeeded,
            ),
        )
        .unwrap();
        finish_job(&p, &row.job_id, "succeeded", None).unwrap();
        for _ in 0..21 {
            let (row, intent, id) = maintenance(&spec, Kind::Scrub);
            insert_job(&p, &row, Some(&intent)).unwrap();
            record_snapraid_result(
                &p,
                &spec.owner,
                &id,
                &super::super::elastic::tests::snapraid_result(
                    &spec,
                    &id,
                    Kind::Scrub,
                    Outcome::Refused,
                ),
            )
            .unwrap();
            finish_job(&p, &row.job_id, "failed", Some("unsynced_changes")).unwrap();
        }
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(array.state, "active");
        assert_eq!(array.snapraid_history.len(), 20);
        assert!(
            array
                .snapraid_history
                .iter()
                .all(|r| r.outcome == "refused")
        );
        assert_eq!(
            array.last_scrub_run.unwrap().operation_id.as_deref(),
            Some(id.as_str())
        );
        let (row, intent, _) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &row, Some(&intent)).unwrap();
    }

    /// MIN-3: the window judges the STORED finish time — the one the query
    /// filtered on — not the finish the helper reported from its own clock.
    /// Two clocks in one comparison is a skew hole: a helper running far enough
    /// ahead would put a run inside the window that the query had already
    /// dropped, and the rows left behind would answer a confident zero.
    #[test]
    fn a_window_row_carries_the_timestamp_the_query_filtered_on() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        let p = pool();
        let spec = completed_array(&p, "clock-skew");
        let (row, intent, id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &row, Some(&intent)).unwrap();
        // The helper reports a finish far OUTSIDE the window; this node stores
        // the row now, INSIDE it.
        let reported = (chrono::Utc::now() - chrono::Duration::days(60))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut result =
            super::super::elastic::tests::snapraid_result(&spec, &id, Kind::Sync, Outcome::Succeeded);
        result.run.started_at = reported.clone();
        result.run.finished_at = Some(reported.clone());
        // `validate_snapraid_result` requires the state's `last_run` to BE this
        // run, and a succeeded Sync to carry it as the sync checkpoint too, so
        // all three move together — only the node's own stored column stays at
        // `now()`, which is the whole point of the case.
        result.state.last_run = Some(result.run.clone());
        result.state.sync_completed_at = Some(reported.clone());
        record_snapraid_result(&p, &spec.owner, &id, &result).unwrap();
        finish_job(&p, &row.job_id, "succeeded", None).unwrap();

        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(
            array.snapraid_history[0].finished_at.as_deref(),
            Some(reported.as_str()),
            "the display history shows the finish the run itself reported"
        );
        let windowed = array
            .parity_window_runs
            .iter()
            .find(|r| r.operation_id.as_deref() == Some(id.as_str()))
            .expect("the run is inside the stored window the query selected");
        assert_ne!(
            windowed.finished_at.as_deref(),
            Some(reported.as_str()),
            "the window row must not be judged on a clock the query never read"
        );
        let stored = chrono::DateTime::parse_from_rfc3339(
            windowed.finished_at.as_deref().expect("stored finish"),
        )
        .expect("RFC3339");
        assert!(
            (chrono::Utc::now() - stored.with_timezone(&chrono::Utc)).num_minutes() < 5,
            "the window row carries this node's stored finish: {:?}",
            windowed.finished_at
        );
    }

    /// The parity window is a DATE query, not the tail of the display history:
    /// it still carries runs the 20-row display list has already rotated away,
    /// and a run leaves it because it aged out — never because newer runs
    /// pushed it past a row count.
    #[test]
    fn the_parity_window_is_selected_by_date_and_not_by_the_history_row_count() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        let p = pool();
        let spec = completed_array(&p, "parity-window");
        // More completed runs than the display history retains.
        let ids: Vec<String> = (0..22)
            .map(|_| {
                let (row, intent, id) = maintenance(&spec, Kind::Sync);
                insert_job(&p, &row, Some(&intent)).unwrap();
                record_snapraid_result(
                    &p,
                    &spec.owner,
                    &id,
                    &super::super::elastic::tests::snapraid_result(
                        &spec,
                        &id,
                        Kind::Sync,
                        Outcome::Succeeded,
                    ),
                )
                .unwrap();
                finish_job(&p, &row.job_id, "succeeded", None).unwrap();
                id
            })
            .collect();
        let carries = |runs: &[tentaflow_protocol::tentanas::NasSnapraidRun], id: &str| {
            runs.iter().any(|r| r.operation_id.as_deref() == Some(id))
        };
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(
            array.snapraid_history.len(),
            20,
            "the display list keeps its own retention, unchanged"
        );
        assert_eq!(
            array.parity_window_runs.len(),
            ids.len(),
            "the window is bounded by date, so it holds every run inside it"
        );
        // MIN-5: an unfinished run is in the display history and NOT in the
        // window — it measures nothing, and 200 of them would otherwise fill
        // the cap and wedge the card to `unknown` with no visible cause.
        let (running_row, running_intent, running_id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &running_row, Some(&running_intent)).unwrap();
        let with_running = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert!(
            with_running
                .snapraid_history
                .iter()
                .any(|r| r.operation_id.as_deref() == Some(running_id.as_str())),
            "a running operation belongs in the display history"
        );
        assert!(
            !with_running
                .parity_window_runs
                .iter()
                .any(|r| r.operation_id.as_deref() == Some(running_id.as_str())),
            "a run with no finish time carries no measurement and must not cost a window row"
        );
        // It stays running on purpose: a row with no finish time is exactly
        // what must not cost a window slot.

        let oldest = ids.first().unwrap().clone();
        assert!(
            !carries(&array.snapraid_history, &oldest),
            "the oldest run has rotated out of the display list"
        );
        assert!(
            carries(&array.parity_window_runs, &oldest),
            "and is still in the parity window, where an error in it cannot hide"
        );
        // Age that run's stored finish past the window: now it leaves — by date.
        let old = (chrono::Utc::now()
            - chrono::Duration::days(
                i64::from(super::super::elastic::PARITY_ERRORS_WINDOW_DAYS) + 5,
            ))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        p.write()
            .unwrap()
            .execute(
                "UPDATE nas_elastic_operations SET finished_at=?1 WHERE operation_id=?2",
                params![old, oldest],
            )
            .unwrap();
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert!(
            !carries(&array.parity_window_runs, &oldest),
            "a run that finished outside the window must not be queried"
        );
        assert_eq!(array.parity_window_runs.len(), ids.len() - 1);
    }

    /// An array a slot can be replaced in, created and settled in the store.
    fn replaceable_array(pool: &DbPool, name: &str) -> ElasticCreateSpec {
        let spec = super::super::elastic::tests::replaceable_spec(name);
        let created = elastic_job(&spec);
        insert_job(pool, &created, Some(&ElasticJobIntent::Create(spec.clone()))).unwrap();
        finish_elastic_operation(
            pool,
            &spec.owner,
            &spec.operation_id,
            Ok(&super::super::elastic::tests::ready_result(&spec)),
        )
        .unwrap();
        finish_job(pool, &created.job_id, "succeeded", None).unwrap();
        spec
    }

    fn replace_job(
        spec: &ElasticCreateSpec,
        branch: &str,
        disk: &ElasticDiskSpec,
    ) -> (NasJob, ElasticJobIntent, String) {
        let mut row = elastic_job(spec);
        row.kind = "elastic_replace_disk".into();
        let operation_id = uuid::Uuid::now_v7().to_string();
        let intent = ElasticJobIntent::ReplaceDisk {
            owner: spec.owner.clone(),
            array_id: spec.array_id.clone(),
            operation_id: operation_id.clone(),
            rebuild_operation_id: uuid::Uuid::now_v7().to_string(),
            sync_operation_id: uuid::Uuid::now_v7().to_string(),
            branch: branch.to_string(),
            disk: disk.clone(),
            accept_stale_parity: false,
        };
        (row, intent, operation_id)
    }

    /// THE STORE REFUSES to open a disk replacement, and this is the gate that
    /// makes "no path may create a `replace_disk` operation" true rather than
    /// hopeful.
    ///
    /// Why the store and not only the handler: a `replace_disk` row that ends
    /// anything but `succeeded` is cleared by nothing an admin can run — only a
    /// succeeded `fix` resolved that kind — so it costs the array every later
    /// Sync, Scrub, mover run and disk addition. The handler refuses first, but
    /// the scheduler, a retry or a stored approval could still reach
    /// `insert_job`, so the intent itself has to be inadmissible. The whole
    /// dormant machinery below (`replace_disk_command`,
    /// `finish_elastic_replace_disk`, the helper's `replace_data_disk`) stays
    /// for the task that finishes the feature.
    #[test]
    fn the_store_refuses_to_open_a_disk_replacement_and_writes_nothing() {
        let p = pool();
        let spec = replaceable_array(&p, "replaced");
        let fresh = super::super::elastic::tests::fresh_disk("replaced", spec.data[0].bytes);
        // The disk is gone: the array needs attention, which is the state the
        // withdrawn feature was asked in.
        p.write()
            .unwrap()
            .execute("UPDATE nas_elastic_arrays SET state='needs_attention'", [])
            .unwrap();
        let jobs_before = list_jobs(&p, 100).unwrap().len();
        let (row, intent, _) = replace_job(&spec, "d1", &fresh);
        let refused = insert_job(&p, &row, Some(&intent)).unwrap_err();
        assert!(
            refused.to_string().contains("Wymiana dysku"),
            "{refused}"
        );

        // Nothing was written: no operation of that kind, no job row, and the
        // array's own spec still names the disk it had.
        let replacements: i64 = p
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM nas_elastic_operations WHERE kind='replace_disk'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(replacements, 0);
        assert_eq!(list_jobs(&p, 100).unwrap().len(), jobs_before, "no job row");
        let array = elastic_array(&p, &spec.owner, "replaced").unwrap().unwrap();
        assert_eq!(array.persisted_spec().unwrap().data[0], spec.data[0]);
        assert_eq!(
            unfinished_elastic_replace_disk(&p, &spec.owner, &spec.array_id).unwrap(),
            None,
            "and nothing is offered as a repeat"
        );

        // AND THE ARRAY IS NOT WEDGED by having asked: a sync is still
        // startable, which is the difference between this refusal and the one
        // the previous version performed after it had written both rows.
        p.write()
            .unwrap()
            .execute("UPDATE nas_elastic_arrays SET state='active'", [])
            .unwrap();
        let (sync, sync_intent, _) = maintenance(
            &array.persisted_spec().unwrap().clone(),
            tentanas_helper::elastic::ElasticSnapraidKind::Sync,
        );
        insert_job(&p, &sync, Some(&sync_intent)).unwrap();
    }

    #[test]
    fn an_unknown_scrub_attempt_does_not_replace_the_last_completed_scrub() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        let p = pool();
        let spec = completed_array(&p, "unknown-scrub");
        let (row, intent, id) = maintenance(&spec, Kind::Scrub);
        insert_job(&p, &row, Some(&intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &id,
            &super::super::elastic::tests::snapraid_result(
                &spec,
                &id,
                Kind::Scrub,
                Outcome::Succeeded,
            ),
        )
        .unwrap();
        finish_job(&p, &row.job_id, "succeeded", None).unwrap();
        let (unknown, intent, _) = maintenance(&spec, Kind::Scrub);
        insert_job(&p, &unknown, Some(&intent)).unwrap();
        finish_job(
            &p,
            &unknown.job_id,
            "failed",
            Some("Utracono odpowiedź helpera"),
        )
        .unwrap();
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(
            array.last_scrub_run.unwrap().operation_id.as_deref(),
            Some(id.as_str())
        );
        assert_eq!(array.snapraid_history[0].outcome, INTERRUPTED_OUTCOME);
        assert!(array.snapraid_history[0].finished_at.is_none());
        assert!(array.snapraid_history[0].checked_blocks.is_none());
    }

    #[test]
    fn failed_scrub_keeps_earlier_sync_receipt_and_blocks_only_the_mover() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        let p = pool();
        let spec = completed_array(&p, "failed-scrub");
        let (row, intent, sync_id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &row, Some(&intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &sync_id,
            &super::super::elastic::tests::snapraid_result(
                &spec,
                &sync_id,
                Kind::Sync,
                Outcome::Succeeded,
            ),
        )
        .unwrap();
        finish_job(&p, &row.job_id, "succeeded", None).unwrap();
        let (row, intent, id) = maintenance(&spec, Kind::Scrub);
        insert_job(&p, &row, Some(&intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &id,
            &super::super::elastic::tests::snapraid_result(
                &spec,
                &id,
                Kind::Scrub,
                Outcome::Failed,
            ),
        )
        .unwrap();
        finish_job(&p, &row.job_id, "failed", Some("data_error")).unwrap();
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(
            array.last_sync_run.unwrap().operation_id.as_deref(),
            Some(sync_id.as_str())
        );
        assert_eq!(array.last_scrub_run.unwrap().errors_data, Some(1));
        // A SYNC IS THE WAY OUT and stays startable; a MOVER is not and does
        // not. Nothing knows what parity currently covers after a scrub that
        // failed, so a run that moved more bytes out of the cache and synced on
        // top of that would be building on an unknown — but the Sync that makes
        // parity current again is precisely what the array needs.
        let (out, out_intent, out_id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &out, Some(&out_intent)).unwrap();
        let blocked_mover = mover_job(&spec);
        let blocked_intent = mover_intent(
            &spec,
            &uuid::Uuid::now_v7().to_string(),
            &uuid::Uuid::now_v7().to_string(),
        );
        assert!(insert_job(&p, &blocked_mover, Some(&blocked_intent)).is_err());
        assert!(job(&p, &blocked_mover.job_id).unwrap().is_none());
        // THE SUCCEEDED SYNC DOES NOT CLEAR THE SCRUB ROW. That scrub counted
        // DATA errors, and a Sync repairs no data: measured on rig11, it leaves
        // a marked block exactly as it was and `-e fix` still recovers it
        // afterwards. Saying the row was settled would put the array back to
        // `active` with the fault still there and a Repair button that could
        // only answer "nothing repaired" (F2 of the fourth review).
        record_snapraid_result(
            &p,
            &spec.owner,
            &out_id,
            &super::super::elastic::tests::snapraid_result(&spec, &out_id, Kind::Sync, Outcome::Succeeded),
        )
        .unwrap();
        finish_job(&p, &out.job_id, "succeeded", None).unwrap();
        let synced = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert!(
            synced.unresolved_operation,
            "a Sync does not settle a scrub that counted data errors"
        );
        // A LATER CLEAN FULL SCRUB does settle it — the product scrubs `-p
        // full`, so a succeeded one is positive evidence over the whole array —
        // and that is the second way out beside the repair.
        let (clean, clean_intent, clean_id) = maintenance(&spec, Kind::Scrub);
        insert_job(&p, &clean, Some(&clean_intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &clean_id,
            &super::super::elastic::tests::snapraid_result(&spec, &clean_id, Kind::Scrub, Outcome::Succeeded),
        )
        .unwrap();
        finish_job(&p, &clean.job_id, "succeeded", None).unwrap();
        let settled = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(settled.state, "active");
        assert!(!settled.unresolved_operation, "a clean full scrub settles the earlier one");
        assert!(settled.parity_run_available);
        let again = mover_job(&spec);
        let again_intent = mover_intent(
            &spec,
            &uuid::Uuid::now_v7().to_string(),
            &uuid::Uuid::now_v7().to_string(),
        );
        insert_job(&p, &again, Some(&again_intent)).unwrap();
    }

    fn pool() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    /// THE REASON `delete_elastic_array` exists: every foreign key into
    /// `nas_elastic_arrays` is `ON DELETE RESTRICT`, and
    /// `block_elastic_teardown` — the first statement of `native_teardown` —
    /// refuses an uninstall while a single array row stands. So before this,
    /// creating one array made the addon permanently un-uninstallable.
    ///
    /// Child-before-parent is not a style choice either: the alias rows key on
    /// the member rows, which key on the array, so any other order is refused
    /// by the keys rather than silently leaving an orphan. The foreign-key
    /// check at the end is what proves nothing was left behind.
    #[test]
    fn deleting_an_array_releases_every_reservation_and_unblocks_the_uninstall() {
        let p = pool();
        let spec = completed_array(&p, "media");
        let other = completed_array(&p, "foto");
        set_elastic_schedule(
            &p,
            &spec.array_id,
            ElasticTask::Sync,
            true,
            &NasSchedule {
                every: "daily".into(),
                hour: 3,
                minute: 30,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        set_mover_settings(&p, &spec.array_id, 3600, 20, true).unwrap();
        assert!(block_elastic_teardown(&p).is_err(), "an array row blocks the uninstall");
        assert_eq!(elastic_claims(&p).unwrap().len(), 4, "two disks per array");

        // An array of ANOTHER owner is not this owner's to release: without the
        // scope one instance's destroy would free another instance's storage.
        let foreign = ElasticOwner {
            org_id: "other-org".into(),
            addon_id: "nas".into(),
        };
        assert!(!delete_elastic_array(&p, &foreign, &spec.array_id).unwrap());
        assert!(elastic_array(&p, &spec.owner, "media").unwrap().is_some());

        assert!(delete_elastic_array(&p, &spec.owner, &spec.array_id).unwrap());
        // Gone once, and only once: a second call is not an error and not a
        // second deletion either.
        assert!(!delete_elastic_array(&p, &spec.owner, &spec.array_id).unwrap());

        assert!(elastic_array(&p, &spec.owner, "media").unwrap().is_none());
        assert_eq!(
            elastic_array_identities(&p).unwrap(),
            vec![(other.array_id.clone(), "foto".to_string())]
        );
        // The DISK RESERVATION is what the destroy actually releases: the
        // remaining claims are the other array's alone.
        let claims = elastic_claims(&p).unwrap();
        assert_eq!(claims.len(), 2);
        assert!(
            !claims.iter().any(|claim| claim.disk_id.starts_with("media-")),
            "{claims:?}"
        );
        for (table, expected) in [
            ("nas_elastic_disks", 2),
            ("nas_elastic_disk_aliases", 6),
            // The other array's default scrub, and nothing of this one's.
            ("nas_elastic_schedules", 1),
            ("nas_elastic_mover_settings", 0),
        ] {
            let left: i64 = p
                .read()
                .unwrap()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(left, expected, "{table}");
        }
        let operations: i64 = p
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM nas_elastic_operations WHERE array_id=?1",
                params![spec.array_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(operations, 0);
        assert!(p
            .read()
            .unwrap()
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none());

        // The other array still holds the instance.
        assert!(block_elastic_teardown(&p).is_err());
        assert!(delete_elastic_array(&p, &other.owner, &other.array_id).unwrap());
        // AND NOW THE UNINSTALL IS NO LONGER BLOCKED.
        block_elastic_teardown(&p).expect("with no array row the teardown proceeds");
    }

    /// The add's two writes are ONE commit: the member row and the array's
    /// persisted intention. `elastic_spec` refuses the array unless the two
    /// agree, so a commit that landed one without the other would make every
    /// later read of the array fail — which is exactly what the read-back
    /// inside the transaction checks.
    #[test]
    fn a_finished_add_makes_the_spec_and_the_member_rows_agree_in_one_commit() {
        let p = pool();
        let spec = completed_array(&p, "grow");
        let added = ElasticDiskSpec {
            disk_id: "grow-data-2".into(),
            wwn: Some("wwn-grow-data-2".into()),
            serial: Some("serial-grow-data-2".into()),
            bytes: 32 * 1024 * 1024 * 1024,
            expected_uuid: uuid::Uuid::new_v4().to_string(),
        };
        let operation_id = uuid::Uuid::now_v7().to_string();
        let mut job = elastic_job(&spec);
        job.kind = "elastic_add_disk".into();
        let intent = ElasticJobIntent::AddDisk {
            owner: spec.owner.clone(),
            array_id: spec.array_id.clone(),
            operation_id: operation_id.clone(),
            disk: added.clone(),
        };
        insert_job(&p, &job, Some(&intent)).unwrap();
        // The operation row is the RESERVATION: while it is open the array's
        // spec is still the array WITHOUT the disk, because `elastic_spec`
        // cross-checks the member rows against that spec.
        let before = elastic_array(&p, &spec.owner, "grow")
            .unwrap()
            .unwrap()
            .persisted_spec()
            .unwrap()
            .clone();
        assert_eq!(before.data.len(), 1);
        // While the add is IN FLIGHT there is nothing to resume: a running
        // operation is not a stopped one, and offering it as resumable is what
        // let a second job race the first over the same slot.
        assert!(
            super::super::db::unfinished_elastic_add_disk(&p, &spec.owner, &spec.array_id)
                .unwrap()
                .is_none(),
            "an add in flight is not offered as resumable"
        );
        // A SECOND disk for the same array is refused while that reservation
        // stands: two adds racing into one slot would format two devices for
        // one place in the array.
        let mut intruder = added.clone();
        intruder.disk_id = "grow-data-3".into();
        intruder.wwn = Some("wwn-grow-data-3".into());
        intruder.serial = Some("serial-grow-data-3".into());
        intruder.expected_uuid = uuid::Uuid::new_v4().to_string();
        let mut second = elastic_job(&spec);
        second.kind = "elastic_add_disk".into();
        assert!(insert_job(
            &p,
            &second,
            Some(&ElasticJobIntent::AddDisk {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: uuid::Uuid::now_v7().to_string(),
                disk: intruder,
            })
        )
        .is_err());

        // A DISK ANOTHER ARRAY OF THIS INSTANCE ALREADY HOLDS is refused in
        // the same transaction that would reserve it, under the very rules the
        // member row's `UNIQUE` columns enforce once the add has closed. This
        // is the "not free" case the handler cannot see from a mount table:
        // an array's branches live in the union process's own namespace, so the
        // host's `/proc/mounts` shows none of them.
        let neighbour = completed_array(&p, "neighbour");
        for taken in [neighbour.data[0].clone(), neighbour.parity[0].clone()] {
            let mut stealing = elastic_job(&spec);
            stealing.kind = "elastic_add_disk".into();
            let error = insert_job(
                &p,
                &stealing,
                Some(&ElasticJobIntent::AddDisk {
                    owner: spec.owner.clone(),
                    array_id: spec.array_id.clone(),
                    operation_id: uuid::Uuid::now_v7().to_string(),
                    disk: taken.clone(),
                }),
            )
            .expect_err("a member of another array is not free");
            assert!(
                error.to_string().contains(&taken.disk_id)
                    || error.to_string().contains("zarezerwowany"),
                "{error}"
            );
            assert!(
                super::super::db::job(&p, &stealing.job_id).unwrap().is_none(),
                "no job row survived"
            );
        }

        let after_spec = super::super::elastic::spec_with_added_disk(&before, &added).unwrap();
        finish_elastic_add_disk(
            &p,
            &spec.owner,
            &operation_id,
            &added,
            &super::super::elastic::tests::ready_result(&after_spec),
        )
        .unwrap();
        finish_job(&p, &job.job_id, "succeeded", None).unwrap();

        let grown = elastic_array(&p, &spec.owner, "grow").unwrap().unwrap();
        assert_eq!(grown.state, "active");
        assert_eq!(grown.persisted_spec().unwrap(), &after_spec);
        assert_eq!(grown.data().count(), 2);
        assert_eq!(grown.data().last().unwrap().name, "d2");
        // The new disk is now a RESERVATION of this instance, the way every
        // other member is: nothing else may claim it.
        assert!(elastic_claims(&p)
            .unwrap()
            .iter()
            .any(|claim| claim.disk_id == added.disk_id));
        assert!(
            super::super::db::unfinished_elastic_add_disk(&p, &spec.owner, &spec.array_id)
                .unwrap()
                .is_none()
        );
        assert!(p
            .read()
            .unwrap()
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none());
    }

    /// AN ADD THAT STOPPED PART-WAY CAN BE FINISHED.
    ///
    /// The helper records the new slot in its own journal before it formats
    /// anything, so repeating the command is what completes the work — and the
    /// repeat must reuse the filesystem UUID the first attempt's mkfs stamped
    /// on the disk, or it would either format a disk that is already a mounted
    /// branch of the array or refuse to mount it. So: the failed attempt is
    /// what HOLDS the identity, a different disk is refused while it stands,
    /// the same disk is admitted even though the array needs attention, and the
    /// superseded attempt stops holding the array.
    #[test]
    fn an_add_that_failed_is_retried_with_the_identity_it_recorded() {
        let p = pool();
        let spec = completed_array(&p, "resume");
        let added = ElasticDiskSpec {
            disk_id: "resume-data-2".into(),
            wwn: Some("wwn-resume-data-2".into()),
            serial: Some("serial-resume-data-2".into()),
            bytes: 32 * 1024 * 1024 * 1024,
            expected_uuid: uuid::Uuid::new_v4().to_string(),
        };
        let start = |disk: &ElasticDiskSpec| {
            let operation_id = uuid::Uuid::now_v7().to_string();
            let mut job = elastic_job(&spec);
            job.kind = "elastic_add_disk".into();
            let intent = ElasticJobIntent::AddDisk {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: operation_id.clone(),
                disk: disk.clone(),
            };
            (job, intent, operation_id)
        };

        let (first_job, first_intent, first_id) = start(&added);
        insert_job(&p, &first_job, Some(&first_intent)).unwrap();

        // A RUNNING add IS NOT A RESUMABLE ONE, and this is the defect that
        // pins it: treating it as one flipped its row out of `running` — which
        // is what the partial unique index refuses a second operation by — and
        // a second helper invocation then raced the first over the same slot,
        // leaving the disk joined to the live union with no member row, because
        // the loser's `finish_elastic_add_disk` finds no running operation of
        // its own. The wait has to be for the verdict.
        let (racing_job, racing_intent, _) = start(&added);
        let racing = insert_job(&p, &racing_job, Some(&racing_intent)).unwrap_err();
        assert!(racing.to_string().contains("jest w toku"), "{racing}");
        assert!(super::super::db::job(&p, &racing_job.job_id).unwrap().is_none());
        // The in-flight row is still running and still holds the array.
        let running: String = p
            .read()
            .unwrap()
            .query_row(
                "SELECT state FROM nas_elastic_operations WHERE operation_id=?1",
                params![first_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(running, "running", "a refused retry may not close the live attempt");
        // And while it runs there is nothing to resume, so the handler cannot
        // take the retry branch and skip the unresolved-operation gate.
        assert!(
            unfinished_elastic_add_disk(&p, &spec.owner, &spec.array_id).unwrap().is_none(),
            "an add in flight is not offered as resumable"
        );

        // The attempt fails the way the job body's error path closes it.
        finish_elastic_operation(&p, &spec.owner, &first_id, Err("mkfs przerwany")).unwrap();
        finish_job(&p, &first_job.job_id, "failed", Some("mkfs przerwany")).unwrap();
        let hurt = elastic_array(&p, &spec.owner, "resume").unwrap().unwrap();
        assert_eq!(hurt.state, "needs_attention");
        assert!(hurt.unresolved_operation);
        assert_eq!(hurt.data().count(), 1, "the member row was never written");

        // A DIFFERENT disk is refused while the recorded identity stands, and
        // the sentence says what to do instead.
        let mut other = added.clone();
        other.disk_id = "resume-data-3".into();
        other.wwn = Some("wwn-resume-data-3".into());
        other.serial = Some("serial-resume-data-3".into());
        other.expected_uuid = uuid::Uuid::new_v4().to_string();
        let (other_job, other_intent, _) = start(&other);
        let refused = insert_job(&p, &other_job, Some(&other_intent)).unwrap_err();
        assert!(
            refused.to_string().contains("powtórz je tym samym dyskiem"),
            "{refused}"
        );

        // THE SAME DISK is admitted, on an array that needs attention.
        assert_eq!(
            unfinished_elastic_add_disk(&p, &spec.owner, &spec.array_id).unwrap().as_ref(),
            Some(&added)
        );
        let (second_job, second_intent, second_id) = start(&added);
        insert_job(&p, &second_job, Some(&second_intent)).unwrap();
        // The retry is now the one in flight, so — like any add in flight — it
        // is not itself offered as resumable until it stops.
        assert!(
            unfinished_elastic_add_disk(&p, &spec.owner, &spec.array_id).unwrap().is_none(),
            "the retry is in flight, not resumable"
        );
        let first_state = || -> String {
            p.read()
                .unwrap()
                .query_row(
                    "SELECT state FROM nas_elastic_operations WHERE operation_id=?1",
                    params![first_id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        // K4: the stopped attempt keeps holding the add while its resume runs
        // — a resume the helper refuses must leave the add pinned.
        assert_eq!(first_state(), "needs_attention", "a resume in flight supersedes nothing yet");

        let after_spec =
            super::super::elastic::spec_with_added_disk(hurt.persisted_spec().unwrap(), &added)
                .unwrap();
        finish_elastic_add_disk(
            &p,
            &spec.owner,
            &second_id,
            &added,
            &super::super::elastic::tests::ready_result(&after_spec),
        )
        .unwrap();
        finish_job(&p, &second_job.job_id, "succeeded", None).unwrap();
        assert_eq!(first_state(), "failed", "the resume's success closes the first attempt as history");

        let grown = elastic_array(&p, &spec.owner, "resume").unwrap().unwrap();
        assert_eq!(grown.state, "active");
        assert_eq!(grown.data().count(), 2);
        assert!(
            !grown.unresolved_operation,
            "nothing is left holding the array once the add has finished"
        );
        assert!(unfinished_elastic_add_disk(&p, &spec.owner, &spec.array_id).unwrap().is_none());
        // And with the array whole again a sync is startable.
        let (sync, sync_intent, _) = maintenance(
            &grown.persisted_spec().unwrap().clone(),
            tentanas_helper::elastic::ElasticSnapraidKind::Sync,
        );
        insert_job(&p, &sync, Some(&sync_intent)).unwrap();
    }

    /// A repair is a first-class operation of the schema: migration 15 admits
    /// its kind, the history reads it back, and a SUCCESSFUL repair resolves
    /// the parity operation that reported the damage — without which the scrub
    /// row that found the errors would block every later sync, scrub and mover
    /// of an array that is demonstrably healthy again.
    #[test]
    fn a_successful_repair_closes_the_operation_that_reported_the_damage() {
        use tentanas_helper::elastic::{ElasticSnapraidKind, ElasticSnapraidOutcome};
        let p = pool();
        let spec = completed_array(&p, "repair");
        // A scrub that found errors: the array needs attention and nothing else
        // may start on it. (A scrub with no result at all is interrupted, which
        // blocks nothing.)
        let (scrub, scrub_intent, scrub_id) = maintenance(&spec, ElasticSnapraidKind::Scrub);
        insert_job(&p, &scrub, Some(&scrub_intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &scrub_id,
            &super::super::elastic::tests::snapraid_result(&spec, &scrub_id, ElasticSnapraidKind::Scrub, ElasticSnapraidOutcome::Failed),
        )
        .unwrap();
        finish_job(&p, &scrub.job_id, "failed", Some("parity errors")).unwrap();
        let hurt = elastic_array(&p, &spec.owner, "repair").unwrap().unwrap();
        assert!(hurt.unresolved_operation);
        assert_eq!(hurt.state, "needs_attention");
        // AND A SYNC IS STILL STARTABLE THERE. It used to be refused, which is
        // the deadlock the fourth review found: the repair's own detail tells
        // the admin to run a Sync, a repair that repairs nothing leaves the
        // scrub row unresolved, and only a SUCCEEDED repair cleared it — so the
        // array had no action left that could get it out. A Sync and a full
        // Scrub are what settle a parity run that ended badly, so they are
        // admitted on exactly the array such a run left behind.
        let (escape, escape_intent, escape_id) = maintenance(&spec, ElasticSnapraidKind::Sync);
        insert_job(&p, &escape, Some(&escape_intent)).unwrap();
        assert!(elastic_array(&p, &spec.owner, "repair").unwrap().unwrap().parity_run_available);
        // It did not have to succeed for the array to stay operable either: the
        // escape is not a one-shot.
        finish_job(&p, &escape.job_id, "failed", Some("utracono odpowiedź helpera")).unwrap();
        let (retry, retry_intent, _) = maintenance(&spec, ElasticSnapraidKind::Scrub);
        insert_job(&p, &retry, Some(&retry_intent)).unwrap();
        finish_job(&p, &retry.job_id, "failed", Some("utracono odpowiedź helpera")).unwrap();
        let _ = escape_id;

        // The REPAIR is admitted there, and its kind reaches the table.
        let (fix, fix_intent, fix_id) = maintenance(
            &spec,
            ElasticSnapraidKind::Fix {
                disk: "d1".to_string(),
            },
        );
        assert_eq!(fix.kind, "elastic_fix");
        insert_job(&p, &fix, Some(&fix_intent)).unwrap();
        let mut result = super::super::elastic::tests::snapraid_result(
            &spec,
            &fix_id,
            ElasticSnapraidKind::Fix {
                disk: "d1".to_string(),
            },
            ElasticSnapraidOutcome::Succeeded,
        );
        // A repair REPORTS what it repaired, so nonzero counters are its work.
        // This is the assertion that would fail if `validate_snapraid_result`
        // held a repair to the three-zero rule a sync and a scrub are held to.
        result.run.errors_data = Some(7);
        result.run.checked_blocks = None;
        result.run.accessed_mb = Some(806);
        // The answer has to BE the array's state: `validate_snapraid_result`
        // refuses a result whose `last_run` is some other run.
        result.state.last_run = Some(result.run.clone());
        record_snapraid_result(&p, &spec.owner, &fix_id, &result).unwrap();
        finish_job(&p, &fix.job_id, "succeeded", None).unwrap();

        let healed = elastic_array(&p, &spec.owner, "repair").unwrap().unwrap();
        assert_eq!(healed.state, "active");
        assert!(
            !healed.unresolved_operation,
            "a successful repair is what resolves the operation that found the errors"
        );
        // The history carries the repair, and names the disk it rebuilt.
        let repair_run = healed
            .snapraid_history
            .iter()
            .find(|run| run.kind == "fix")
            .expect("the repair is in the history");
        assert_eq!(repair_run.outcome, "ok");
        assert_eq!(repair_run.errors_data, Some(7));
        // The scrub row is NOT rewritten: a failed scrub happened, and the
        // history keeps saying so. What the repair changed is only whether it
        // still holds the array.
        let scrub_row = healed
            .snapraid_history
            .iter()
            .find(|run| run.operation_id.as_deref() == Some(scrub_id.as_str()))
            .expect("the failed scrub stays in the history");
        assert_eq!(scrub_row.outcome, "failed");
        // And a sync is startable again.
        let (again, again_intent, _) = maintenance(&spec, ElasticSnapraidKind::Sync);
        insert_job(&p, &again, Some(&again_intent)).unwrap();
    }

    /// A SUCCEEDED REPAIR RESOLVES PARITY RUNS AND NOTHING ELSE.
    ///
    /// It used to resolve every unresolved operation of the array, because the
    /// supersede clause matched on the newer row's success alone: a repair then
    /// cleared a mover run that stopped part-way and an add-disk that failed —
    /// operations parity cannot repair and whose state nothing had settled. The
    /// clause now pairs kinds: a succeeded parity run supersedes parity runs, a
    /// succeeded mover supersedes movers, and a failed add-disk keeps holding
    /// the array until an add finishes it.
    #[test]
    fn a_repair_resolves_the_parity_run_it_healed_and_not_a_stuck_mover() {
        use tentanas_helper::elastic::{ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome};
        let p = pool();
        let spec = completed_array(&p, "scoped-fix");
        // A mover that stopped part-way: its row is unresolved, and only a
        // mover can settle it.
        let mover = uuid::Uuid::now_v7().to_string();
        let resume = uuid::Uuid::now_v7().to_string();
        let mover_row = mover_job(&spec);
        insert_job(&p, &mover_row, Some(&mover_intent(&spec, &mover, &resume))).unwrap();
        finish_job(&p, &mover_row.job_id, "failed", Some("przerwane w trakcie przenoszenia")).unwrap();
        // A SYNC OR A SCRUB CANNOT EVEN START over it, which is the rule that
        // makes this the only order this state can be reached in: an unresolved
        // mover means nothing knows what parity covers.
        let (blocked, blocked_intent, _) = maintenance(&spec, Kind::Scrub);
        assert!(insert_job(&p, &blocked, Some(&blocked_intent)).is_err());
        // A REPAIR is admitted there — it writes back blocks a scrub marked and
        // needs no mover to have finished — and it succeeds.
        let fix_kind = Kind::Fix { disk: "d1".to_string() };
        let (fix, fix_intent, fix_id) = maintenance(&spec, fix_kind.clone());
        insert_job(&p, &fix, Some(&fix_intent)).unwrap();
        let mut result =
            super::super::elastic::tests::snapraid_result(&spec, &fix_id, fix_kind, Outcome::Succeeded);
        result.run.errors_data = Some(3);
        result.run.checked_blocks = None;
        result.state.last_run = Some(result.run.clone());
        record_snapraid_result(&p, &spec.owner, &fix_id, &result).unwrap();
        finish_job(&p, &fix.job_id, "succeeded", None).unwrap();

        // The MOVER row is untouched, and the array says so.
        let healed = elastic_array(&p, &spec.owner, "scoped-fix").unwrap().unwrap();
        assert!(
            healed.unresolved_operation,
            "a repair does not settle a mover that stopped part-way"
        );
        assert_eq!(
            healed.snapraid_history.iter().filter(|run| run.kind == "fix").count(),
            1,
            "and the repair itself is recorded"
        );
        assert!(
            !healed.parity_run_available,
            "and an unresolved mover still holds every parity run"
        );
        let unresolved: Vec<String> = p
            .read()
            .unwrap()
            .prepare(
                "SELECT kind FROM nas_elastic_operations
                 WHERE array_id=?1 AND state IN ('failed','needs_attention','running')
                   AND NOT EXISTS(
                     SELECT 1 FROM nas_elastic_operations f
                     WHERE f.array_id = nas_elastic_operations.array_id AND f.state = 'succeeded'
                       AND ((nas_elastic_operations.kind IN ('sync','scrub','fix') AND f.kind IN ('fix','sync','scrub'))
                            OR (nas_elastic_operations.kind = 'mover' AND f.kind = 'mover'))
                       AND (f.created_at, f.operation_id) > (nas_elastic_operations.created_at, nas_elastic_operations.operation_id))",
            )
            .unwrap()
            .query_map(params![spec.array_id], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(unresolved, vec!["mover".to_string()], "only the mover is left");

        // And a succeeded MOVER is what settles it — after which every action
        // is back, the repair having settled the parity side already.
        let second = uuid::Uuid::now_v7().to_string();
        let second_resume = uuid::Uuid::now_v7().to_string();
        let second_row = mover_job(&spec);
        insert_job(&p, &second_row, Some(&mover_intent(&spec, &second, &second_resume))).unwrap();
        let mover_result = super::super::elastic::tests::mover_result(
            &spec,
            &second,
            &second_resume,
            tentanas_helper::elastic::ElasticMoverPhase::Complete,
            true,
        );
        record_mover_result(&p, &spec.owner, &second, &mover_result).unwrap();
        finish_job(&p, &second_row.job_id, "succeeded", None).unwrap();
        let settled = elastic_array(&p, &spec.owner, "scoped-fix").unwrap().unwrap();
        assert!(!settled.unresolved_operation);
        assert!(settled.parity_run_available);
    }

    /// WHICH SUCCEEDED RUN SETTLES WHICH ROW: an interrupted repair is not
    /// settled by a Sync, and a scrub that could only report unreadable files
    /// is — because for that one a Sync is the only cure the product has.
    ///
    /// The any-to-any version of the clause let a Sync close both, which put
    /// the array back to `active` with its fault intact and left the Repair
    /// button armed on evidence only a repair could clear — and after twenty
    /// later runs the row aged out of the history and the arming vanished with
    /// the fault still there (F2 of the fourth review).
    #[test]
    fn a_sync_settles_a_scrub_that_only_lost_files_and_never_an_interrupted_repair() {
        use tentanas_helper::elastic::{ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome};
        let p = pool();
        let spec = completed_array(&p, "scoped-settle");
        let fix_kind = Kind::Fix { disk: "d1".to_string() };

        // AN INTERRUPTED REPAIR: the job never reported. It may have rewritten
        // part of a disk, so it stays unresolved.
        let (fix, fix_intent, _) = maintenance(&spec, fix_kind.clone());
        insert_job(&p, &fix, Some(&fix_intent)).unwrap();
        finish_job(&p, &fix.job_id, "failed", Some("utracono odpowiedź helpera")).unwrap();
        assert!(elastic_array(&p, &spec.owner, "scoped-settle").unwrap().unwrap().unresolved_operation);

        // A succeeded SYNC is no evidence about it.
        let (sync, sync_intent, sync_id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &sync, Some(&sync_intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &sync_id,
            &super::super::elastic::tests::snapraid_result(&spec, &sync_id, Kind::Sync, Outcome::Succeeded),
        )
        .unwrap();
        finish_job(&p, &sync.job_id, "succeeded", None).unwrap();
        assert!(
            elastic_array(&p, &spec.owner, "scoped-settle").unwrap().unwrap().unresolved_operation,
            "a Sync does not settle an interrupted repair"
        );

        // A CLEAN FULL SCRUB is, and it is reachable: a scrub is admitted on
        // the array the interrupted repair left behind.
        let (scrub, scrub_intent, scrub_id) = maintenance(&spec, Kind::Scrub);
        insert_job(&p, &scrub, Some(&scrub_intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &scrub_id,
            &super::super::elastic::tests::snapraid_result(&spec, &scrub_id, Kind::Scrub, Outcome::Succeeded),
        )
        .unwrap();
        finish_job(&p, &scrub.job_id, "succeeded", None).unwrap();
        assert!(!elastic_array(&p, &spec.owner, "scoped-settle").unwrap().unwrap().unresolved_operation);

        // A SCRUB THAT ONLY LOST FILES: no data error was counted, so there is
        // no marked block for a repair to write back — measured on rig11,
        // `-e fix` reports `error:0 recovered:0` for unreadable files. The Sync
        // that removes them from the content file is the cure, and it settles
        // the row; otherwise the array would keep a fault nothing could clear.
        let (lost, lost_intent, lost_id) = maintenance(&spec, Kind::Scrub);
        insert_job(&p, &lost, Some(&lost_intent)).unwrap();
        let mut only_files =
            super::super::elastic::tests::snapraid_result(&spec, &lost_id, Kind::Scrub, Outcome::Failed);
        only_files.run.errors_file = Some(245);
        only_files.run.errors_data = Some(0);
        only_files.state.last_run = Some(only_files.run.clone());
        record_snapraid_result(&p, &spec.owner, &lost_id, &only_files).unwrap();
        finish_job(&p, &lost.job_id, "failed", Some("file errors")).unwrap();
        let hurt = elastic_array(&p, &spec.owner, "scoped-settle").unwrap().unwrap();
        assert!(hurt.unresolved_operation);
        assert_eq!(hurt.snapraid_history[0].errors, Some(245));

        let (cure, cure_intent, cure_id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &cure, Some(&cure_intent)).unwrap();
        record_snapraid_result(
            &p,
            &spec.owner,
            &cure_id,
            &super::super::elastic::tests::snapraid_result(&spec, &cure_id, Kind::Sync, Outcome::Succeeded),
        )
        .unwrap();
        finish_job(&p, &cure.job_id, "succeeded", None).unwrap();
        let cured = elastic_array(&p, &spec.owner, "scoped-settle").unwrap().unwrap();
        assert!(
            !cured.unresolved_operation,
            "a Sync settles a scrub that counted no data errors"
        );
        assert_eq!(cured.state, "active");
    }

    /// A REFUSAL BY THE VERSION GATE LEAVES THE ARRAY ALONE, for a parity run
    /// and for a mover alike.
    ///
    /// The gate refuses every Elastic command while the installed helper is not
    /// the build this core speaks to (`broker::HELPER_VERSION_MARKER`), and the
    /// window in which that happens is unattended: the helper is provisioned by
    /// hand after a core upgrade, so the cadence keeps firing in between. If
    /// each refused tick parked `needs_attention` on the array, the upgrade
    /// window would cost the array its Sync, its Scrub and its mover as well —
    /// so a refusal is `failed` with nothing touched, exactly like the helper
    /// being busy.
    #[test]
    fn a_helper_version_refusal_is_recorded_as_nothing_ran() {
        use tentanas_helper::elastic::ElasticSnapraidKind as Kind;
        let p = pool();
        let spec = completed_array(&p, "skew");
        let refusal = format!(
            "{}: zainstalowany helper ma wersję 0.12.0, a ten rdzeń wymaga 0.13.0",
            super::super::broker::HELPER_VERSION_MARKER
        );

        // A scheduled Sync, refused before it reached the helper.
        let (sync, sync_intent, sync_id) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &sync, Some(&sync_intent)).unwrap();
        finish_job(&p, &sync.job_id, "failed", Some(&refusal)).unwrap();
        let after = elastic_array(&p, &spec.owner, "skew").unwrap().unwrap();
        assert_eq!(after.state, "active", "the array is untouched");
        assert!(!after.unresolved_operation, "a refusal is not a fault to resolve");
        assert!(after.parity_run_available);
        let state: String = p
            .read()
            .unwrap()
            .query_row(
                "SELECT state FROM nas_elastic_operations WHERE operation_id=?1",
                params![sync_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "failed");
        // The history says REFUSED, not "failed with no counters": nothing ran,
        // so there is nothing to count.
        assert_eq!(after.snapraid_history[0].outcome, "refused");
        assert!(after.snapraid_history[0].errors.is_none());

        // And the mover's side of the same refusal.
        let mover = uuid::Uuid::now_v7().to_string();
        let resume = uuid::Uuid::now_v7().to_string();
        let row = mover_job(&spec);
        insert_job(&p, &row, Some(&mover_intent(&spec, &mover, &resume))).unwrap();
        finish_job(&p, &row.job_id, "failed", Some(&refusal)).unwrap();
        let after = elastic_array(&p, &spec.owner, "skew").unwrap().unwrap();
        assert_eq!(after.state, "active");
        assert!(!after.unresolved_operation, "a refused mover holds nothing either");

        // So the very next run — once the admin has re-provisioned — is
        // admitted with no intervening repair, sync or acknowledgement.
        let (again, again_intent, _) = maintenance(&spec, Kind::Sync);
        insert_job(&p, &again, Some(&again_intent)).unwrap();
    }

    /// A second self-test on one disk ABORTS the first, so `insert_job`
    /// refuses it — for the scheduler's short pass, for the manual start and
    /// for two rapid clicks on it alike, because this is the only place where
    /// the check and the insert are one atomic step.
    ///
    /// The refusal is scoped to the disk, to the kind and to a test that is
    /// still running: another disk, another kind and a finished test all stay
    /// startable. Without the scoping this would refuse legitimate work,
    /// because `insert_job` opens EVERY job of the node.
    #[test]
    fn a_second_running_smart_test_on_one_disk_is_refused() {
        let p = pool();
        let mk = |id: &str, kind: &str, subject: &str, status: &str| NasJob {
            job_id: id.into(),
            kind: kind.into(),
            subject: subject.into(),
            status: status.into(),
            started_by: "test".into(),
            started_at: now(),
            ..Default::default()
        };
        insert_job(&p, &mk("j1", "smart_test", "disk-a", "running"), None).unwrap();

        // The manual path, or the short pass, arriving at a disk already under test.
        let refused = insert_job(&p, &mk("j2", "smart_test", "disk-a", "running"), None)
            .expect_err("a second test on disk-a is refused");
        assert!(
            refused.to_string().contains("autotest SMART"),
            "{refused}"
        );
        let rows: i64 = p
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM nas_jobs WHERE subject = 'disk-a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1, "the refused job left no row behind");

        // Everything the refusal must NOT touch.
        insert_job(&p, &mk("j3", "smart_test", "disk-b", "running"), None)
            .expect("another disk is free");
        insert_job(&p, &mk("j4", "scrub", "disk-a", "running"), None)
            .expect("another kind does not serialise on the disk");
        insert_job(&p, &mk("j5", "smart_test", "disk-c", "succeeded"), None)
            .expect("a finished test is not a running one");
        insert_job(&p, &mk("j6", "smart_test", "disk-c", "running"), None)
            .expect("a finished test does not block the next one");
    }

    /// Migration 16 in the shape migration 13 used: a NEW table, so nothing is
    /// rebuilt and every existing row and index has to come through untouched.
    #[test]
    fn schema_sixteen_adds_the_folder_cache_and_keeps_every_row_and_index() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..15]).unwrap();
        let array_id = "16161616-1616-4616-8616-161616161616";
        conn.execute(
            "INSERT INTO nas_elastic_arrays
             (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
             VALUES (?1,'org','addon','legacy','xfs','active','','now','now')",
            params![array_id],
        )
        .unwrap();
        // A v15 database with everything the other Elastic tables can hold, so
        // a migration that touched them would be visible here.
        conn.execute(
            "INSERT INTO nas_elastic_mover_settings
             (array_id,min_age_secs,cache_min_free_pct,coupled_sync) VALUES (?1,7200,20,1)",
            params![array_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_schedules (array_id,kind,enabled,schedule_json)
             VALUES (?1,'mover',1,'{}')",
            params![array_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at,log)
             VALUES ('job-create','elastic_create','legacy','succeeded','test','now','')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nas_elastic_operations
             (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
             VALUES ('op-create',?1,'job-create','create','succeeded','{}','','2026-09-01T00:00:00Z')",
            params![array_id],
        )
        .unwrap();
        let snapshot = |conn: &Connection| -> (Vec<String>, Vec<(String, i64, i64, i64)>, Vec<(String, String)>) {
            let indexes = conn
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='index'
                     AND name NOT LIKE 'sqlite_%' ORDER BY name",
                )
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<String>>>()
                .unwrap();
            let settings = conn
                .prepare("SELECT array_id,min_age_secs,cache_min_free_pct,coupled_sync
                          FROM nas_elastic_mover_settings ORDER BY array_id")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let operations = conn
                .prepare("SELECT operation_id,state FROM nas_elastic_operations ORDER BY operation_id")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (indexes, settings, operations)
        };
        let before = snapshot(&conn);
        let folder_table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name='nas_elastic_folder_cache'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(folder_table, 0, "v15 has nowhere to put a folder policy");

        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();

        // What migration 16 must not do is change what EXISTED: every index
        // that was there must still be there, and the rows must be untouched.
        // Later migrations are free to ADD indexes (migration 20 adds the
        // owner indexes), so the index list is compared as a superset, not
        // for equality — equality made this test fail on every new index.
        let (indexes_after, settings_after, operations_after) = snapshot(&conn);
        let (indexes_before, settings_before, operations_before) = before;
        let lost: Vec<&String> = indexes_before.iter().filter(|i| !indexes_after.contains(i)).collect();
        assert!(lost.is_empty(), "migracja 16 usunęła istniejące indeksy: {lost:?}");
        assert_eq!(settings_after, settings_before, "migracja 16 zmieniła istniejące dane");
        assert_eq!(operations_after, operations_before, "migracja 16 zmieniła istniejące dane");
        conn.execute(
            "INSERT INTO nas_elastic_folder_cache (array_id,folder,cache_policy)
             VALUES (?1,'foto','only')",
            params![array_id],
        )
        .unwrap();
        // 'yes' is the DEFAULT and the default is the absence of a row, so the
        // schema itself refuses a second spelling of it. The same CHECK is what
        // keeps a path out of a column the mover resolves under the union.
        for (folder, policy) in [
            ("foto2", "yes"),
            ("a/b", "only"),
            ("..", "only"),
            ("", "only"),
            ("foto3", "maybe"),
        ] {
            assert!(
                conn.execute(
                    "INSERT INTO nas_elastic_folder_cache (array_id,folder,cache_policy)
                     VALUES (?1,?2,?3)",
                    params![array_id, folder, policy],
                )
                .is_err(),
                "schema przyjęła ({folder}, {policy})"
            );
        }
        // The foreign key is the array's, so a policy for an array nobody has
        // cannot be written at all.
        assert!(conn
            .execute(
                "INSERT INTO nas_elastic_folder_cache (array_id,folder,cache_policy)
                 VALUES ('nie-ma','foto','no')",
                [],
            )
            .is_err());
    }

    /// The clause this whole slice exists for: a policy an admin stored is what
    /// the mover is carried out under, and a folder nobody decided for is
    /// `yes`. Read through the REAL store, not through a hand-built row.
    #[test]
    fn a_stored_policy_becomes_a_mover_rule_and_an_absent_one_stays_the_default() {
        use super::super::elastic::CachePolicy;
        let p = pool();
        let spec = completed_array(&p, "folder-policy");
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        // `/mnt/folder-policy` is not a mounted union on a test host, so the
        // discovery half is UNKNOWN. That is not "no folders" — see
        // `an_unreadable_union_is_unknown_and_never_zero_folders`.
        assert!(!array.folders_known);
        assert!(array.mover_rules().pinned_folders.is_empty());
        assert!(array.mover_rules().eager_folders.is_empty());

        assert!(set_elastic_folder_policy(&p, &spec.array_id, "foto", CachePolicy::Only).unwrap());
        assert!(set_elastic_folder_policy(&p, &spec.array_id, "backup", CachePolicy::No).unwrap());
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        let rules = array.mover_rules();
        assert_eq!(rules.pinned_folders, vec!["foto".to_string()]);
        assert_eq!(rules.eager_folders, vec!["backup".to_string()]);
        // A stored policy is listed even while the union cannot be read: it is
        // an intention the run has to honour, not a directory listing.
        assert!(!array.folders_known);
        assert_eq!(array.folders.len(), 2);
        // And the helper accepts what we derived, so a policy that can be
        // stored can always be run.
        rules.validate().expect("the helper accepts these rules");

        // Back to the default: the row goes away rather than turning into a
        // third stored value that could disagree with the absence of one.
        assert!(set_elastic_folder_policy(&p, &spec.array_id, "foto", CachePolicy::Yes).unwrap());
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert!(array.mover_rules().pinned_folders.is_empty());
        assert_eq!(array.mover_rules().eager_folders, vec!["backup".to_string()]);
        let stored: i64 = p
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM nas_elastic_folder_cache WHERE array_id=?1",
                params![spec.array_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, 1, "domyślna polityka nie zostawia wiersza");
    }

    /// The one spelling of the default scrub the migration uses and the one
    /// the create path uses are the same schedule, byte for byte — a drift
    /// would give upgraded arrays and new arrays different defaults.
    #[test]
    fn the_default_scrub_json_is_the_default_scrub_schedule() {
        assert_eq!(
            serde_json::to_string(&default_elastic_scrub_schedule()).unwrap(),
            default_elastic_scrub_schedule_json!()
        );
        let schedule = default_elastic_scrub_schedule();
        assert_eq!(schedule.every, "monthly");
        // A day every month has, so the cadence never skips a month.
        assert!((1..=28).contains(&schedule.day));
    }

    /// Owner decision 2026-09-22: a NEW array is scrubbed monthly without
    /// anyone arming it — the row is written with the array row itself.
    #[test]
    fn new_array_gets_the_default_scrub_schedule() {
        let p = pool();
        let spec = super::super::elastic::tests::create_spec("media");
        insert_job(&p, &elastic_job(&spec), Some(&ElasticJobIntent::Create(spec.clone()))).unwrap();
        // Already while the create runs: same transaction as the array row.
        let row = elastic_schedule(&p, &spec.array_id, ElasticTask::Scrub).unwrap().expect("default scrub");
        assert!(row.enabled);
        assert_eq!(row.schedule, default_elastic_scrub_schedule());
        // Never fires retroactively: the scheduler computes the first slot.
        assert_eq!(row.next_run_at, None);
        assert!(row.last_run_at.is_none());
        // Only the scrub: sync is already coupled to the mover, and the mover
        // cadence is the admin's.
        for task in [ElasticTask::Mover, ElasticTask::Sync] {
            assert!(elastic_schedule(&p, &spec.array_id, task).unwrap().is_none(), "{}", task.kind());
        }

        // An array WITHOUT parity has nothing to scrub against, and `insert_job`
        // would refuse the run every month — so it gets no default.
        let mut bare = super::super::elastic::tests::create_spec("bare");
        bare.parity.clear();
        insert_job(&p, &elastic_job(&bare), Some(&ElasticJobIntent::Create(bare.clone()))).unwrap();
        assert!(elastic_schedule(&p, &bare.array_id, ElasticTask::Scrub).unwrap().is_none());
    }

    /// An ADOPTED array is a new row to this node, and gets what a created
    /// one gets.
    #[test]
    fn adopted_array_gets_the_default_scrub_schedule() {
        let p = pool();
        let spec = super::super::elastic::tests::create_spec("media");
        elastic_import(&p, &spec, "admin").unwrap();
        let row = elastic_schedule(&p, &spec.array_id, ElasticTask::Scrub).unwrap().expect("default scrub");
        assert!(row.enabled);
        assert_eq!(row.schedule, default_elastic_scrub_schedule());
    }

    /// Migration 19: an array that existed before the upgrade and has no scrub
    /// schedule gets the default ONCE. One whose admin already decided keeps
    /// the decision, one without parity gets nothing, and a default that is
    /// later removed is not recreated by any later start of the node.
    #[test]
    fn existing_array_without_scrub_gets_the_default_once() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..18]).unwrap();
        let array = |id: &str, name: &str, roles: &[&str]| {
            conn.execute(
                "INSERT INTO nas_elastic_arrays
                 (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
                 VALUES (?1,'org','addon',?2,'xfs','active','','now','now')",
                params![id, name],
            )
            .unwrap();
            for role in roles {
                conn.execute(
                    "INSERT INTO nas_elastic_disks
                     (array_id,role,slot,disk_id,wwn,serial,bytes,expected_uuid)
                     VALUES (?1,?2,1,?3,NULL,?3,1024,?4)",
                    params![id, role, format!("{name}-{role}"), uuid::Uuid::new_v4().to_string()],
                )
                .unwrap();
            }
        };
        // `produkt` on rig11: data + parity + cache, no scrub schedule at all.
        array("a-plain", "produkt", &["data", "parity", "cache"]);
        // An admin already chose: scrub switched OFF, weekly.
        array("a-chosen", "chosen", &["data", "parity"]);
        let weekly = r#"{"every":"weekly","hour":3,"minute":0,"weekday":3,"day":1}"#;
        conn.execute(
            "INSERT INTO nas_elastic_schedules (array_id,kind,enabled,schedule_json)
             VALUES ('a-chosen','scrub',0,?1)",
            params![weekly],
        )
        .unwrap();
        // No parity: nothing to scrub against.
        array("a-bare", "bare", &["data"]);

        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();

        let scrub = |conn: &Connection, id: &str| -> Option<(i64, String, Option<String>)> {
            conn.query_row(
                "SELECT enabled, schedule_json, next_run_at FROM nas_elastic_schedules
                 WHERE array_id=?1 AND kind='scrub'",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .unwrap()
        };
        assert_eq!(
            scrub(&conn, "a-plain"),
            Some((1, default_elastic_scrub_schedule_json!().to_string(), None)),
            "the array without a scrub gets the default, not yet armed with a slot"
        );
        assert_eq!(
            scrub(&conn, "a-chosen"),
            Some((0, weekly.to_string(), None)),
            "an admin's decision is never overwritten"
        );
        assert_eq!(scrub(&conn, "a-bare"), None, "no parity, no default");
        // Nothing but scrub rows was written.
        let others: i64 = conn
            .query_row("SELECT COUNT(*) FROM nas_elastic_schedules WHERE kind<>'scrub'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(others, 0);

        // A DELETED DEFAULT IS NOT RECREATED: the node starts again (the same
        // migration runner every open goes through) and the row stays gone —
        // version 19 is recorded and never runs twice.
        conn.execute("DELETE FROM nas_elastic_schedules WHERE array_id='a-plain'", []).unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(scrub(&conn, "a-plain"), None, "a removed default must stay removed");
        assert_eq!(scrub(&conn, "a-chosen"), Some((0, weekly.to_string(), None)));
    }

    /// The admin's later choice sticks: a default switched off or re-timed is
    /// never re-armed — not by a restart of the node, and not by anything else
    /// that reads or writes the array.
    #[test]
    fn a_disabled_default_scrub_is_not_rearmed() {
        let p = pool();
        let spec = completed_array(&p, "media");
        let weekly = NasSchedule { every: "weekly".into(), hour: 2, minute: 0, weekday: 6, day: 1 };
        set_elastic_schedule(&p, &spec.array_id, ElasticTask::Scrub, false, &weekly, None).unwrap();
        // A second array created afterwards gets ITS default and touches
        // nobody else's row.
        let other = completed_array(&p, "foto");
        migrate(&p.write().unwrap()).unwrap();

        let row = elastic_schedule(&p, &spec.array_id, ElasticTask::Scrub).unwrap().unwrap();
        assert!(!row.enabled, "switched off stays off");
        assert_eq!(row.schedule, weekly, "re-timed stays re-timed");
        let array = elastic_arrays(&p, &spec.owner)
            .unwrap()
            .into_iter()
            .find(|a| a.name == "media")
            .unwrap();
        assert!(!array.snapraid.scrub_enabled);
        assert_eq!(array.snapraid.scrub_schedule, Some(weekly));
        assert!(elastic_schedule(&p, &other.array_id, ElasticTask::Scrub).unwrap().unwrap().enabled);
    }

    /// Dissolving an array takes its folder policies with it. A leftover row
    /// holds the array's foreign key, so the next `delete` of an array that
    /// reused the id would be refused by the key rather than by anything that
    /// could explain itself.
    #[test]
    fn deleting_an_array_leaves_no_folder_policy_rows() {
        use super::super::elastic::CachePolicy;
        let p = pool();
        let spec = completed_array(&p, "folder-orphans");
        assert!(set_elastic_folder_policy(&p, &spec.array_id, "foto", CachePolicy::Only).unwrap());
        assert!(set_elastic_folder_policy(&p, &spec.array_id, "backup", CachePolicy::No).unwrap());
        set_mover_settings(&p, &spec.array_id, 3600, 25, true).unwrap();

        assert!(delete_elastic_array(&p, &spec.owner, &spec.array_id).unwrap());

        let conn = p.read().unwrap();
        for table in [
            "nas_elastic_folder_cache",
            "nas_elastic_mover_settings",
            "nas_elastic_schedules",
            "nas_elastic_disks",
            "nas_elastic_operations",
        ] {
            let left: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(left, 0, "{table} zostawiła sierotę");
        }
    }

    /// The ceiling is the helper's own, enforced where the admin can read it.
    #[test]
    fn the_folder_policy_limit_refuses_the_hundred_and_twenty_ninth_folder() {
        use super::super::elastic::CachePolicy;
        let p = pool();
        let spec = completed_array(&p, "folder-limit");
        for i in 0..FOLDER_POLICY_LIMIT {
            assert!(
                set_elastic_folder_policy(&p, &spec.array_id, &format!("f{i}"), CachePolicy::Only)
                    .unwrap(),
                "folder {i} mieści się w limicie"
            );
        }
        assert!(
            !set_elastic_folder_policy(&p, &spec.array_id, "jeszcze-jeden", CachePolicy::No)
                .unwrap(),
            "limit musi odmówić, nie zapisać"
        );
        // A refusal writes NOTHING, and an edit of a folder already inside the
        // limit still works — it is not a new row.
        let array = elastic_array(&p, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(array.folders.len(), FOLDER_POLICY_LIMIT as usize);
        array.mover_rules().validate().expect("helper accepts a full set");
        assert!(set_elastic_folder_policy(&p, &spec.array_id, "f0", CachePolicy::No).unwrap());
    }

    #[test]
    fn migration_is_idempotent_and_tables_exist() {
        let p = pool();
        let conn = p.write().unwrap();
        migrate(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'nas_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Migracje 1–8 mają 19 tabel; schema9 dodaje cztery tabele Elastic,
        // schema13 harmonogramy Elastic i ustawienia movera (E2-10),
        // a schema16 polityki cache folderów.
        assert_eq!(n, 26);
    }

    #[test]
    fn a_target_keeps_its_structure_and_its_allowlist_across_a_round_trip() {
        let p = pool();
        let row = TargetRow {
            target_id: "t1".into(),
            name: "vm-store".into(),
            protocol: "iscsi".into(),
            wwn: "iqn.2026-09.pl.euvic:helios.vm-store".into(),
            enabled: true,
            luns: vec![NasTargetLun {
                index: 0,
                source: "tank/vm-store".into(),
                device_path: "/dev/zvol/tank/vm-store".into(),
                size_bytes: 2_199_023_255_552,
                thin: true,
                uuid: "0191f2c0-0000-7000-8000-000000000001".into(),
                group_id: 7,
                source_kind: "zvol".into(),
            }],
            portals: vec![NasTargetPortal {
                interface: "storage0".into(),
                address: "10.10.0.5".into(),
                port: 3260,
                transport: "iser".into(),
            }],
            port_groups: vec![NasTargetPortGroup {
                group_id: 7,
                state: "non-optimized".into(),
                preferred: true,
            }],
            // Out of order on purpose: the read is what makes the generated
            // configfs stable, not the caller.
            initiators: vec![
                "iqn.1998-01.com.vmware:esx02".into(),
                "iqn.1998-01.com.vmware:esx01".into(),
            ],
            auth_method: "mutual-chap".into(),
            auth_username: "vmware01".into(),
            auth_secret: "encb:ciphertext-one".into(),
            auth_mutual_username: "helios".into(),
            auth_mutual_secret: "encb:ciphertext-two".into(),
            dhchap_hash: String::new(),
            dhchap_dhgroup: String::new(),
            state: "disabled".into(),
            state_detail: String::new(),
            created_at: now(),
            updated_at: now(),
        };
        upsert_target(&p, "org-a", &row).unwrap();

        let back = target(&p, "org-a", "t1").unwrap().expect("target");
        assert_eq!(back.luns, row.luns);
        assert_eq!(back.portals, row.portals);
        // The ALUA/ANA port group state survives the database (R8).
        assert_eq!(back.port_groups, row.port_groups);
        assert_eq!(
            back.initiators,
            vec![
                "iqn.1998-01.com.vmware:esx01".to_string(),
                "iqn.1998-01.com.vmware:esx02".to_string(),
            ]
        );
        // What is stored is the ciphertext, never a plaintext secret.
        assert_eq!(back.auth_secret, "encb:ciphertext-one");
        assert_eq!(back.auth_mutual_secret, "encb:ciphertext-two");

        set_target_state(&p, "t1", "active", "").unwrap();
        assert_eq!(list_targets(&p).unwrap()[0].state, "active");
        assert_eq!(target_counts(&p, "org-a").unwrap(), (1, 0));
        set_target_state(&p, "t1", "error", "nvmet missing").unwrap();
        assert_eq!(target_counts(&p, "org-a").unwrap(), (1, 1));

        // A second target may not claim the same name or the same WWN: both
        // are also configfs object names.
        let clash = TargetRow {
            target_id: "t2".into(),
            ..row.clone()
        };
        assert!(upsert_target(&p, "org-a", &clash).is_err());

        assert!(delete_target(&p, "org-a", "t1").unwrap());
        assert!(target(&p, "org-a", "t1").unwrap().is_none());
        assert!(target_initiators(&p, "t1").unwrap().is_empty());
    }

    #[test]
    fn retention_rolls_minutes_into_hours_and_drops_what_is_past_the_window() {
        let p = pool();
        let now = chrono::Utc::now();
        let at = |ago: chrono::Duration| {
            (now - ago).format("%Y-%m-%dT%H:%M:00Z").to_string()
        };
        // Two minutes of ONE hour that is entirely past the 48 h window (both
        // pinned to the same hour, so the assertion below does not depend on
        // what minute the test happens to run at), one minute inside the
        // window, and one sample older than the 30-day history.
        let old_hour = (now - chrono::Duration::hours(50))
            .format("%Y-%m-%dT%H")
            .to_string();
        let old_a = format!("{old_hour}:10:00Z");
        let old_b = format!("{old_hour}:20:00Z");
        let fresh = at(chrono::Duration::minutes(5));
        let ancient = at(chrono::Duration::days(40));
        let rows: [(&str, i32, u64, u64); 4] = [
            (&old_a, 40, 1000, 4),
            (&old_b, 44, 3000, 6),
            (&fresh, 41, 7000, 2),
            (&ancient, 39, 100, 1),
        ];
        let samples: Vec<SampleInsert<'_>> = rows
            .iter()
            .map(|&(at, temp, bps, realloc)| SampleInsert {
                disk_id: "d1",
                at,
                temperature_c: Some(temp),
                reallocated: Some(realloc),
                pending: None,
                crc_errors: None,
                media_errors: None,
                read_bps: bps,
                write_bps: 0,
                await_ms: 1.0,
            })
            .collect();
        insert_samples(&p, &samples).unwrap();
        prune_samples(&p).unwrap();

        // The minute table keeps only the fresh sample…
        let minutes = samples_since(&p, "d1", "1970-01-01T00:00:00Z").unwrap();
        assert_eq!(minutes.len(), 1);
        assert_eq!(minutes[0].at, fresh);

        // …the whole history still reaches back past the minute window, with
        // the two old minutes averaged into one hourly row.
        let history = history_since(&p, "d1", "1970-01-01T00:00:00Z").unwrap();
        assert_eq!(history.len(), 2, "{history:?}");
        assert_eq!(history[0].read_bps, 2000);
        assert_eq!(history[0].temperature_c, Some(42));
        // A monotonic counter takes the hour's maximum, never its average.
        assert_eq!(history[0].reallocated_sectors, Some(6));
        assert_eq!(history[1].at, fresh);
        // The 40-day-old sample is gone from both tables, not downsampled.
        assert!(history.iter().all(|s| s.at != ancient));
    }

    #[test]
    fn a_share_keeps_its_grants_in_their_own_table() {
        let p = pool();
        let mut share = ShareRow {
            share_id: "s1".into(),
            name: "projekty".into(),
            protocol: "smb".into(),
            source_path: "/mnt/tank/projekty".into(),
            dataset: Some("tank/projekty".into()),
            enabled: true,
            fleet_mount: true,
            smb: Some(NasSmbOptions {
                guests: false,
                previous_versions: true,
                recycle_bin: true,
                time_machine: false,
                smb_direct: false,
                audit: false,
                audit_groups: Vec::new(),
                audit_success: false,
                audit_failure: false,
                users: vec![
                    NasShareAccess {
                        user: "anna".into(),
                        mode: "rw".into(),
                    },
                    NasShareAccess {
                        user: "jan".into(),
                        mode: "ro".into(),
                    },
                ],
            }),
            nfs: None,
            state: "active".into(),
            state_detail: String::new(),
            created_at: now(),
            updated_at: now(),
        };
        upsert_share_user(&p, "org-a", "anna", "projekt lead").unwrap();
        upsert_share_user(&p, "org-a", "jan", "").unwrap();
        upsert_share(&p, "org-a", &share).unwrap();
        let back = list_shares(&p).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].smb.as_ref().unwrap().users.len(), 2);
        assert!(back[0].smb.as_ref().unwrap().previous_versions);
        // The options blob never carries the grants, so there is one truth.
        let raw: String = p
            .read()
            .unwrap()
            .query_row("SELECT options_json FROM nas_shares", [], |r| r.get(0))
            .unwrap();
        assert!(!raw.contains("anna"), "{raw}");

        // A rewrite replaces the grants instead of adding to them.
        share.smb.as_mut().unwrap().users.pop();
        upsert_share(&p, "org-a", &share).unwrap();
        assert_eq!(share_grants(&p, "s1").unwrap().len(), 1);

        let users = list_share_users(&p, "org-a").unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].name, "anna");
        assert_eq!(users[0].shares, vec!["projekty".to_string()]);
        assert!(users[1].shares.is_empty(), "jan lost his grant");

        // Deleting a user takes its grants with it.
        assert!(delete_share_user(&p, "org-a", "anna").unwrap());
        assert!(share_grants(&p, "s1").unwrap().is_empty());
        assert!(!delete_share_user(&p, "org-a", "anna").unwrap());

        set_share_state(&p, "s1", "error", "source path is not mounted").unwrap();
        assert_eq!(share_counts(&p, "org-a").unwrap(), (1, 1));
        assert!(delete_share(&p, "org-a", "s1").unwrap());
        assert_eq!(share_counts(&p, "org-a").unwrap(), (0, 0));
        assert!(share_by_name(&p, "projekty").unwrap().is_none());
    }

    #[test]
    fn alert_dedupe_keeps_one_open_row_per_key() {
        let p = pool();
        assert!(raise_alert(&p, "disk:a:temp", "warning", "disk", "a", "hot", "").unwrap());
        assert!(!raise_alert(&p, "disk:a:temp", "warning", "disk", "a", "hot", "").unwrap());
        resolve_alert(&p, "disk:a:temp").unwrap();
        assert!(raise_alert(&p, "disk:a:temp", "warning", "disk", "a", "hot", "").unwrap());
        assert_eq!(list_alerts(&p, false).unwrap().len(), 1);
        let id = list_alerts(&p, false).unwrap()[0].alert_id.clone();
        assert!(ack_alert(&p, &id).unwrap());
        assert_eq!(list_alerts(&p, false).unwrap().len(), 0);
        assert_eq!(list_alerts(&p, true).unwrap().len(), 1);
    }

    #[test]
    fn job_log_round_trips_as_lines() {
        let p = pool();
        let j = NasJob {
            job_id: "j1".into(),
            kind: "packages_install".into(),
            subject: "zfs".into(),
            status: "running".into(),
            progress_pct: None,
            started_by: "u1".into(),
            started_at: now(),
            finished_at: None,
            error: None,
            log: vec![],
            subject_last_known: false,
        };
        insert_job(&p, &j, None).unwrap();
        append_job_log(&p, "j1", "first").unwrap();
        append_job_log(&p, "j1", "second").unwrap();
        finish_job(&p, "j1", "succeeded", None).unwrap();
        let got = job(&p, "j1").unwrap().unwrap();
        assert_eq!(got.log, vec!["first", "second"]);
        assert_eq!(got.progress_pct, Some(100));
        assert_eq!(fail_orphaned_jobs(&p).unwrap(), 0);
    }

    fn weekly() -> NasSchedule {
        NasSchedule {
            every: "weekly".into(),
            hour: 2,
            minute: 0,
            weekday: 0,
            day: 1,
        }
    }

    /// Both pool tasks (§5.10 added the trim) live in their own table with
    /// the same row shape, and neither can see the other's rows.
    #[test]
    fn a_pool_schedule_survives_a_rewrite_and_records_its_runs() {
        let p = pool();
        for task in [PoolTask::Scrub, PoolTask::Trim] {
            assert!(pool_schedule(&p, task, "tank").unwrap().is_none());
            set_pool_schedule(&p, task, "tank", true, &weekly(), Some("2026-09-06T00:00:00Z"))
                .unwrap();
            let row = pool_schedule(&p, task, "tank").unwrap().unwrap();
            assert!(row.enabled);
            assert_eq!(row.schedule, weekly());
            assert_eq!(row.next_run_at.as_deref(), Some("2026-09-06T00:00:00Z"));
            assert!(row.last_run_at.is_none());

            record_pool_schedule_run(&p, task, "tank", "started job j1", Some("2026-09-13T00:00:00Z"))
                .unwrap();
            let row = pool_schedule(&p, task, "tank").unwrap().unwrap();
            assert_eq!(row.last_result, "started job j1");
            assert!(row.last_run_at.is_some());
            assert_eq!(row.next_run_at.as_deref(), Some("2026-09-13T00:00:00Z"));

            // Disabling rewrites the same row rather than adding a second one.
            set_pool_schedule(&p, task, "tank", false, &weekly(), None).unwrap();
            assert_eq!(list_pool_schedules(&p, task).unwrap().len(), 1);
            assert!(!list_pool_schedules(&p, task).unwrap()[0].enabled);
        }
        // A destroyed pool takes both of its schedules with it.
        delete_pool_schedules(&p, "tank").unwrap();
        assert!(list_pool_schedules(&p, PoolTask::Scrub).unwrap().is_empty());
        assert!(list_pool_schedules(&p, PoolTask::Trim).unwrap().is_empty());
    }

    /// The three Elastic cadences share one table and one row shape, keyed by
    /// (array, kind). The mover's RULES are a row of their own, and that is
    /// what lets `configured` answer honestly: a saved cadence is not a
    /// decision about the age and free-space rules.
    #[test]
    fn elastic_schedules_and_mover_settings_persist_per_array_and_kind() {
        let p = pool();
        let spec = super::super::elastic::tests::create_spec("media");
        let job = elastic_job(&spec);
        insert_job(&p, &job, Some(&ElasticJobIntent::Create(spec.clone()))).unwrap();
        finish_elastic_operation(
            &p,
            &spec.owner,
            &spec.operation_id,
            Ok(&super::super::elastic::tests::ready_result(&spec)),
        )
        .unwrap();
        finish_job(&p, &job.job_id, "succeeded", None).unwrap();

        // Nothing configured by anyone: no mover or sync cadence, no decision
        // claimed — and the one default the product arms by itself, the
        // monthly scrub (owner decision 2026-09-22).
        let before = elastic_arrays(&p, &spec.owner).unwrap().remove(0);
        assert!(!before.mover.configured);
        assert!(!before.mover.schedule_enabled, "no row means no restriction on automatic moves");
        assert_eq!(before.mover.schedule, None);
        assert_eq!(before.snapraid.sync_schedule, None);
        assert_eq!(before.snapraid.scrub_schedule, Some(default_elastic_scrub_schedule()));
        assert!(before.snapraid.scrub_enabled);

        for task in ElasticTask::ALL {
            assert_eq!(
                elastic_schedule(&p, &spec.array_id, task).unwrap().is_some(),
                task == ElasticTask::Scrub,
                "{}",
                task.kind()
            );
            set_elastic_schedule(
                &p,
                &spec.array_id,
                task,
                true,
                &weekly(),
                Some("2026-09-06T00:00:00Z"),
            )
            .unwrap();
        }
        for task in ElasticTask::ALL {
            let row = elastic_schedule(&p, &spec.array_id, task).unwrap().unwrap();
            assert_eq!(row.kind, task.kind());
            assert!(row.enabled);
            assert_eq!(row.schedule, weekly());
            assert!(row.last_run_at.is_none());
        }

        // Recording one kind's run leaves the other two alone — three rows,
        // not one row three verbs share.
        record_elastic_schedule_run(
            &p,
            &spec.array_id,
            ElasticTask::Mover,
            "started job j1",
            Some("2026-09-13T00:00:00Z"),
        )
        .unwrap();
        let row = elastic_schedule(&p, &spec.array_id, ElasticTask::Mover)
            .unwrap()
            .unwrap();
        assert_eq!(row.last_result, "started job j1");
        assert!(row.last_run_at.is_some());
        assert_eq!(row.next_run_at.as_deref(), Some("2026-09-13T00:00:00Z"));
        assert!(elastic_schedule(&p, &spec.array_id, ElasticTask::Sync)
            .unwrap()
            .unwrap()
            .last_run_at
            .is_none());

        // A cadence alone does NOT make the rules somebody's decision.
        let armed = elastic_arrays(&p, &spec.owner).unwrap().remove(0);
        assert!(armed.mover.schedule_enabled);
        assert_eq!(armed.mover.schedule, Some(weekly()));
        assert!(
            !armed.mover.configured,
            "a saved cadence is not a decision about the rules"
        );
        let defaults = super::super::elastic::MoverConfig::default();
        assert_eq!(armed.mover.min_age_secs, defaults.min_age_secs);
        assert_eq!(armed.mover.cache_min_free_pct, defaults.cache_min_free_pct);
        assert_eq!(armed.snapraid.sync_schedule, Some(weekly()));
        assert_eq!(armed.snapraid.scrub_schedule, Some(weekly()));

        // Saving the rules is what flips `configured`, and the values are the
        // ones written rather than the defaults.
        set_mover_settings(&p, &spec.array_id, 1800, 35, false).unwrap();
        let tuned = elastic_arrays(&p, &spec.owner).unwrap().remove(0);
        assert!(tuned.mover.configured);
        assert_eq!(tuned.mover.min_age_secs, 1800);
        assert_eq!(tuned.mover.cache_min_free_pct, 35);
        assert!(!tuned.mover.coupled_sync);

        // Rewriting a cadence rewrites its row rather than adding a second,
        // and a disabled cadence stays SAVED while arming nothing.
        set_elastic_schedule(&p, &spec.array_id, ElasticTask::Mover, false, &weekly(), None)
            .unwrap();
        assert_eq!(list_elastic_schedules(&p, ElasticTask::Mover).unwrap().len(), 1);
        let off = elastic_arrays(&p, &spec.owner).unwrap().remove(0);
        assert!(!off.mover.schedule_enabled);
        assert_eq!(off.mover.schedule, Some(weekly()));
        assert!(off.mover.configured, "the rules survive disabling the cadence");
    }

    #[test]
    fn one_snapshot_schedule_per_dataset() {
        let p = pool();
        let mut s = NasSnapshotSchedule {
            schedule_id: "s1".into(),
            dataset: "tank/projekty".into(),
            enabled: true,
            recursive: true,
            schedule: NasSchedule {
                every: "15m".into(),
                ..Default::default()
            },
            keep_frequent: 96,
            keep_daily: 30,
            keep_monthly: 12,
            protect_days: 7,
            ..Default::default()
        };
        upsert_snapshot_schedule(&p, &s, Some("2026-09-01T14:45:00Z")).unwrap();
        // A second write for the same dataset replaces the first.
        s.schedule_id = "s2".into();
        s.keep_frequent = 48;
        upsert_snapshot_schedule(&p, &s, None).unwrap();
        let all = list_snapshot_schedules(&p).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].schedule_id, "s1", "the dataset keeps its original id");
        assert_eq!(all[0].keep_frequent, 48);
        assert_eq!(all[0].keep_daily, 30);
        assert_eq!(all[0].protect_days, 7);
        assert!(all[0].recursive);

        record_snapshot_run(&p, "s1", "ok", Some("2026-09-01T15:00:00Z")).unwrap();
        assert_eq!(snapshot_schedule_result(&p, "s1").unwrap(), "ok");
        assert!(snapshot_schedule(&p, "s1").unwrap().unwrap().last_run_at.is_some());
        assert!(delete_snapshot_schedule(&p, "s1").unwrap());
        assert!(!delete_snapshot_schedule(&p, "s1").unwrap());
    }

    #[test]
    fn a_protection_record_is_one_row_per_snapshot_and_the_last_write_wins() {
        let p = pool();
        assert!(snapshot_protection(&p).unwrap().is_empty());
        record_snapshot_protection(
            &p,
            "tank/projekty@przed-migracja",
            30,
            "2026-10-01T14:45:00Z",
            "anna",
            false,
        )
        .unwrap();
        // Extending the same snapshot's protection replaces the row; only an
        // approved release ever removes it, so a later date is the only edit.
        record_snapshot_protection(
            &p,
            "tank/projekty@przed-migracja",
            90,
            "2026-12-01T14:45:00Z",
            "anna",
            true,
        )
        .unwrap();
        record_snapshot_protection(
            &p,
            "tank/backups@kwartal",
            365,
            "2027-09-01T00:00:00Z",
            "piotr",
            false,
        )
        .unwrap();
        let all = snapshot_protection(&p).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.get("tank/projekty@przed-migracja").map(String::as_str),
            Some("2026-12-01T14:45:00Z")
        );
        assert_eq!(
            all.get("tank/backups@kwartal").map(String::as_str),
            Some("2027-09-01T00:00:00Z")
        );
        // How the hold was placed decides how it comes off; a snapshot with no
        // record of ours is released as narrowly as we know how.
        assert!(snapshot_protection_recursive(&p, "tank/projekty@przed-migracja").unwrap());
        assert!(!snapshot_protection_recursive(&p, "tank/backups@kwartal").unwrap());
        assert!(!snapshot_protection_recursive(&p, "tank/obce@reczny").unwrap());

        // An approved release forgets the record, so the UI stops claiming a
        // protection ZFS no longer has.
        forget_snapshot_protection(&p, "tank/projekty@przed-migracja").unwrap();
        let all = snapshot_protection(&p).unwrap();
        assert_eq!(all.len(), 1);
        assert!(!all.contains_key("tank/projekty@przed-migracja"));
    }

    #[test]
    fn a_parked_operation_is_decided_once_and_expires_on_its_own_deadline() {
        let p = pool();
        let row = |request_id: &str, expires_at: &str| ApprovalRow {
            approval: tentaflow_protocol::tentanas::NasPendingApproval {
                request_id: request_id.to_string(),
                operation: "pool_destroy".to_string(),
                subject: "tank".to_string(),
                detail: "niszczy pulę tank".to_string(),
                status: "pending".to_string(),
                requested_by: "u-anna".to_string(),
                requested_at: "2026-09-03T10:00:00Z".to_string(),
                expires_at: expires_at.to_string(),
                ..Default::default()
            },
            payload_json: "{\"PoolDestroyRequest\":{\"name\":\"tank\"}}".to_string(),
            org_id: "org-1".to_string(),
            addon_id: "tentanas-1".to_string(),
        };
        insert_approval(&p, &row("r-open", "2999-01-01T00:00:00Z")).unwrap();
        insert_approval(&p, &row("r-late", "2020-01-01T00:00:00Z")).unwrap();

        assert_eq!(list_approvals(&p, "org-1", false).unwrap().len(), 2);
        let due = approvals_past_ttl(&p, &now()).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].approval.request_id, "r-late");
        assert_eq!(due[0].payload_json, row("x", "y").payload_json);

        // The first decision wins; the second changes nothing at all.
        assert!(close_approval(&p, "r-open", "approved", Some("u-piotr"), "").unwrap());
        assert!(!close_approval(&p, "r-open", "rejected", Some("u-jan"), "za późno").unwrap());
        let decided = approval(&p, "r-open").unwrap().unwrap().approval;
        assert_eq!(decided.status, "approved");
        assert_eq!(decided.decided_by.as_deref(), Some("u-piotr"));
        assert!(decided.decided_at.is_some());

        set_approval_outcome(&p, "r-open", "approved", Some("job-1")).unwrap();
        assert_eq!(
            approval(&p, "r-open").unwrap().unwrap().approval.decision_job_id.as_deref(),
            Some("job-1")
        );
        // Only the still-open one is listed by default; both show with history.
        assert_eq!(
            list_approvals(&p, "org-1", false).unwrap().into_iter().map(|r| r.approval.request_id).collect::<Vec<_>>(),
            vec!["r-late"]
        );
        assert_eq!(list_approvals(&p, "org-1", true).unwrap().len(), 2);
        assert!(approval(&p, "nie-ma").unwrap().is_none());
    }

    #[test]
    fn the_approval_list_is_one_organisations_and_capped_after_the_filter() {
        // The organisation is part of the query: with the cap applied first,
        // another tenant's requests could fill it and hide the caller's own.
        let p = pool();
        let row = |request_id: &str, org: &str, at: &str| ApprovalRow {
            approval: tentaflow_protocol::tentanas::NasPendingApproval {
                request_id: request_id.to_string(),
                operation: "pool_destroy".to_string(),
                subject: "tank".to_string(),
                status: "pending".to_string(),
                requested_by: "u-anna".to_string(),
                requested_at: at.to_string(),
                expires_at: "2999-01-01T00:00:00Z".to_string(),
                ..Default::default()
            },
            payload_json: "{}".to_string(),
            org_id: org.to_string(),
            addon_id: "tentanas-1".to_string(),
        };
        // The caller's one request is OLDER than 200 newer ones of another org.
        insert_approval(&p, &row("mine", "org-1", "2026-09-01T00:00:00Z")).unwrap();
        for i in 0..200 {
            insert_approval(&p, &row(&format!("theirs-{i}"), "org-2", "2026-09-02T00:00:00Z")).unwrap();
        }
        let mine = list_approvals(&p, "org-1", true).unwrap();
        assert_eq!(mine.iter().map(|r| r.approval.request_id.as_str()).collect::<Vec<_>>(), vec!["mine"]);
        assert!(list_approvals(&p, "org-2", true).unwrap().iter().all(|r| r.org_id == "org-2"));
        assert!(list_approvals(&p, "", true).unwrap().is_empty(), "an empty org owns nothing");
    }

    #[test]
    fn pool_samples_fold_latency_into_the_shared_sample_shape() {
        let p = pool();
        insert_pool_samples(
            &p,
            &[PoolSampleInsert {
                pool: "tank",
                sampled_at: "2026-09-01T14:45:00Z",
                read_bps: 335_544_320,
                write_bps: 146_800_640,
                read_iops: 1420.0,
                write_iops: 420.0,
                read_latency_ms: 2.8,
                write_latency_ms: 6.1,
            }],
        )
        .unwrap();
        let rows = pool_samples_since(&p, "tank", "2026-09-01T00:00:00Z").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].read_bps, 335_544_320);
        // (1420*2.8 + 420*6.1) / 1840 = 3.55…
        assert!((rows[0].await_ms - 3.55).abs() < 0.01, "{}", rows[0].await_ms);
        // A pool has no temperature and no sectors of its own.
        assert_eq!(rows[0].temperature_c, None);
        assert_eq!(rows[0].reallocated_sectors, None);
        assert!(pool_samples_since(&p, "tank", "2026-09-02T00:00:00Z").unwrap().is_empty());
        // Everything in the fixture is older than 24 h from now.
        assert_eq!(prune_pool_samples(&p).unwrap(), 1);
    }

    #[test]
    fn the_smart_schedule_defaults_to_disabled_and_round_trips() {
        let p = pool();
        let empty = smart_schedule(&p).unwrap();
        assert!(!empty.enabled);
        let smart = NasSmartSchedule {
            enabled: true,
            short: NasSchedule {
                every: "daily".into(),
                hour: 1,
                ..Default::default()
            },
            long: NasSchedule {
                every: "monthly".into(),
                hour: 1,
                minute: 30,
                day: 1,
                ..Default::default()
            },
            next_short_at: Some("2026-09-02T01:00:00Z".into()),
            ..Default::default()
        };
        set_smart_schedule(&p, &smart).unwrap();
        assert_eq!(smart_schedule(&p).unwrap(), smart);
    }

    // ----- tenants (migration 18) ---------------------------------------------------

    /// One array per organisation, created the way the product creates one,
    /// so the job and the array row come from the real write path.
    fn tenant_array(p: &DbPool, org: &str, name: &str) -> ElasticCreateSpec {
        let mut spec = super::super::elastic::tests::create_spec(name);
        spec.owner = ElasticOwner { org_id: org.into(), addon_id: "tentanas-shared".into() };
        insert_job(p, &elastic_job(&spec), Some(&ElasticJobIntent::Create(spec.clone()))).unwrap();
        spec
    }

    fn plain_job(kind: &str, subject: &str) -> NasJob {
        NasJob { job_id: uuid::Uuid::now_v7().to_string(), kind: kind.into(), subject: subject.into(),
            status: "running".into(), started_by: "test".into(), started_at: now(), ..Default::default() }
    }

    fn subjects(jobs: &[NasJob]) -> std::collections::BTreeSet<String> {
        jobs.iter().map(|j| format!("{}:{}", j.kind, j.subject)).collect()
    }

    #[test]
    fn a_tenant_sees_its_own_and_the_node_wide_jobs_but_never_another_organisations_array_jobs() {
        let p = pool();
        tenant_array(&p, "org-a", "alpha");
        tenant_array(&p, "org-b", "bravo");
        // An Elastic job WITHOUT an intent is stamped by its array name too.
        let bravo_mover = plain_job("elastic_mover", "bravo");
        insert_job(&p, &bravo_mover, None).unwrap();
        let scrub = plain_job("pool_scrub", "tank");
        insert_job(&p, &scrub, None).unwrap();

        let a = subjects(&list_jobs_for_org(&p, "org-a", 100).unwrap());
        assert_eq!(a, ["elastic_create:alpha", "pool_scrub:tank"].map(String::from).into_iter().collect());
        let b = subjects(&list_jobs_for_org(&p, "org-b", 100).unwrap());
        assert_eq!(b, ["elastic_create:bravo", "elastic_mover:bravo", "pool_scrub:tank"]
            .map(String::from).into_iter().collect());
        // A guessed id of the other tenant's job answers like an unknown id.
        assert!(job_for_org(&p, "org-a", &bravo_mover.job_id).unwrap().is_none());
        assert!(job_for_org(&p, "org-b", &bravo_mover.job_id).unwrap().is_some());
        assert!(job_for_org(&p, "org-a", &scrub.job_id).unwrap().is_some(), "shared hardware stays shared");
        // The node's own loops still see every job.
        assert_eq!(list_jobs(&p, 100).unwrap().len(), 4);
        // No organisation id is empty, but an empty one must not match the
        // unowned rows either.
        assert_eq!(subjects(&list_jobs_for_org(&p, "", 100).unwrap()),
            ["pool_scrub:tank"].map(String::from).into_iter().collect());
    }

    #[test]
    fn an_adopted_array_job_belongs_to_the_adopting_organisation() {
        let p = pool();
        let mut spec = super::super::elastic::tests::create_spec("media");
        spec.owner = ElasticOwner { org_id: "org-a".into(), addon_id: "tentanas-shared".into() };
        elastic_import(&p, &spec, "admin").unwrap();
        assert_eq!(list_jobs_for_org(&p, "org-a", 100).unwrap().len(), 1);
        assert!(list_jobs_for_org(&p, "org-b", 100).unwrap().is_empty());
    }

    #[test]
    fn an_alert_about_another_organisations_array_is_invisible_and_cannot_be_acked() {
        let p = pool();
        tenant_array(&p, "org-a", "alpha");
        tenant_array(&p, "org-b", "bravo");
        raise_alert(&p, "elastic:alpha:x", "warning", "elastic-array", "alpha", "Macierz alpha", "").unwrap();
        raise_alert(&p, "elastic:bravo:x", "warning", "elastic-array", "bravo", "Macierz bravo", "").unwrap();
        raise_alert(&p, "disk:d1:temp", "warning", "disk", "d1", "Disk sda: hot", "").unwrap();

        let seen = |org: &str| -> std::collections::BTreeSet<String> {
            list_alerts_for_org(&p, org, true).unwrap().into_iter().map(|a| a.subject_id).collect()
        };
        assert_eq!(seen("org-a"), ["alpha", "d1"].map(String::from).into_iter().collect());
        assert_eq!(seen("org-b"), ["bravo", "d1"].map(String::from).into_iter().collect());

        let bravo = list_alerts_for_org(&p, "org-b", true).unwrap()
            .into_iter().find(|a| a.subject_id == "bravo").unwrap().alert_id;
        assert!(!ack_alert_for_org(&p, "org-a", &bravo).unwrap(), "not this tenant's to acknowledge");
        assert!(list_alerts_for_org(&p, "org-b", false).unwrap().iter().any(|a| a.alert_id == bravo));
        assert!(ack_alert_for_org(&p, "org-b", &bravo).unwrap());
        // The node-wide read (forwarding, the scheduler) still has all three.
        assert_eq!(list_alerts(&p, true).unwrap().len(), 3);
    }

    #[test]
    fn a_parked_request_alert_belongs_to_the_organisation_that_parked_it() {
        let p = pool();
        let row = ApprovalRow {
            payload_json: "{}".into(),
            org_id: "org-a".into(),
            addon_id: "tentanas-shared".into(),
            approval: tentaflow_protocol::tentanas::NasPendingApproval {
                request_id: "req-1".into(), operation: "elastic_destroy".into(), subject: "alpha".into(),
                status: "pending".into(), requested_by: "u".into(), requested_at: now(), expires_at: now(),
                ..Default::default()
            },
        };
        insert_approval(&p, &row).unwrap();
        raise_alert(&p, "approval:req-1", "warning", "approval", "req-1", "a red-path operation on 'alpha'", "").unwrap();
        assert_eq!(list_alerts_for_org(&p, "org-a", true).unwrap().len(), 1);
        assert!(list_alerts_for_org(&p, "org-b", true).unwrap().is_empty());
    }

    #[test]
    fn an_array_job_or_alert_whose_array_cannot_be_found_is_shown_to_no_tenant() {
        let p = pool();
        insert_job(&p, &plain_job("elastic_sync", "ghost"), None).unwrap();
        raise_alert(&p, "elastic:ghost:x", "warning", "elastic-array", "ghost", "Macierz ghost", "").unwrap();
        for org in ["org-a", "org-b"] {
            assert!(list_jobs_for_org(&p, org, 100).unwrap().is_empty(), "{org}");
            assert!(list_alerts_for_org(&p, org, true).unwrap().is_empty(), "{org}");
        }
        assert_eq!(list_jobs(&p, 100).unwrap().len(), 1, "the row itself is kept");
    }

    /// A job created WITH an owner (`insert_job_owned`, what `spawn_owned`
    /// writes for a journal-releasing disk wipe) is that organisation's from
    /// the moment the row exists: the INSERT carries it, so no read between
    /// two writes can find it node-wide. The owner rule stays one rule: an
    /// Elastic job's owner is its array's and a caller may not name another.
    #[test]
    fn a_job_inserted_with_an_owner_is_never_node_wide() {
        let p = pool();
        let wipe = plain_job("disk_wipe", "sdq");
        insert_job_owned(&p, &wipe, None, Some("org-a")).unwrap();
        // The row as first written: owned, not NULL.
        let stored: Option<String> = p.read().unwrap()
            .query_row("SELECT org_id FROM nas_jobs WHERE job_id = ?1", params![wipe.job_id], |r| r.get(0))
            .unwrap();
        assert_eq!(stored.as_deref(), Some("org-a"));
        assert!(list_jobs_for_org(&p, "org-b", 100).unwrap().is_empty());
        assert_eq!(list_jobs_for_org(&p, "org-a", 100).unwrap().len(), 1);

        // Without an owner a wipe is shared hardware, as before.
        let plain = plain_job("disk_wipe", "sdr");
        insert_job(&p, &plain, None).unwrap();
        assert_eq!(list_jobs_for_org(&p, "org-b", 100).unwrap().len(), 1, "node-wide");

        // One rule for Elastic jobs: the array decides, an explicit owner is
        // refused and writes nothing.
        tenant_array(&p, "org-b", "bravo");
        let mover = plain_job("elastic_mover", "bravo");
        assert!(insert_job_owned(&p, &mover, None, Some("org-a")).is_err());
        assert!(job_for_org(&p, "org-b", &mover.job_id).unwrap().is_none(), "nothing was inserted");
        insert_job(&p, &mover, None).unwrap();
        assert!(job_for_org(&p, "org-b", &mover.job_id).unwrap().is_some());
        // An empty organisation id is nobody, not an owner.
        assert!(insert_job_owned(&p, &plain_job("disk_wipe", "sds"), None, Some("")).is_err());
    }

    /// Migration 22 rebuilds `nas_elastic_operations` to admit the undo's
    /// kind. A node upgrading from 21 keeps every row as it was — state,
    /// request, result, error, times — and every index: the one running
    /// operation per array and the one origin per array are still enforced,
    /// and the new kind is accepted while an unknown one is still refused.
    #[test]
    fn migration_22_keeps_every_operation_row_and_admits_the_undo() {
        let conn = Connection::open_in_memory().unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..21]).unwrap();
        conn.execute_batch(
            "INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
               VALUES ('arr-a','org-a','nas','alpha','xfs','needs_attention','x','now','now');
             INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at,log) VALUES
               ('j-create','elastic_create','alpha','succeeded','u','now',''),
               ('j-add','elastic_add_disk','alpha','failed','u','now',''),
               ('j-sync','elastic_sync','alpha','failed','u','now',''),
               ('j-undo','elastic_add_disk_abort','alpha','running','u','now',''),
               ('j-bad','elastic_sync','alpha','running','u','now',''),
               ('j-run','elastic_scrub','alpha','running','u','now','');
             INSERT INTO nas_elastic_operations
               (operation_id,array_id,job_id,kind,state,request_json,result_json,error,created_at,finished_at) VALUES
               ('op-1','arr-a','j-create','create','succeeded','{\"c\":1}',NULL,'','t1',NULL),
               ('op-2','arr-a','j-add','add_disk','needs_attention','{\"a\":2}','{\"r\":2}','mkfs','t2','t3'),
               ('op-3','arr-a','j-sync','sync','failed','{\"s\":3}',NULL,'refusal:elastic_no_parity','t4','t5');",
        )
        .unwrap();
        let before: Vec<(String, String, String, String, Option<String>, String, String, Option<String>)> = {
            let mut statement = conn
                .prepare("SELECT operation_id,kind,state,request_json,result_json,error,created_at,finished_at
                          FROM nas_elastic_operations ORDER BY operation_id")
                .unwrap();
            statement
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        // Before 22 the undo's kind is refused.
        assert!(conn
            .execute(
                "INSERT INTO nas_elastic_operations (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
                 VALUES ('op-x','arr-a','j-undo','add_disk_abort','running','{}','','t6')",
                [],
            )
            .is_err());
        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();
        let after: Vec<(String, String, String, String, Option<String>, String, String, Option<String>)> = {
            let mut statement = conn
                .prepare("SELECT operation_id,kind,state,request_json,result_json,error,created_at,finished_at
                          FROM nas_elastic_operations ORDER BY operation_id")
                .unwrap();
            statement
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(after, before, "every row survives the rebuild unchanged");
        conn.execute(
            "INSERT INTO nas_elastic_operations (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
             VALUES ('op-4','arr-a','j-undo','add_disk_abort','running','{}','','t6')",
            [],
        )
        .expect("the undo's kind is admitted");
        // The indexes are back: a second running operation, a second origin
        // and an unknown kind are refused.
        assert!(conn
            .execute(
                "INSERT INTO nas_elastic_operations (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
                 VALUES ('op-5','arr-a','j-run','scrub','running','{}','','t7')",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO nas_elastic_operations (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
                 VALUES ('op-6','arr-a','j-run','import','succeeded','{}','','t7')",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO nas_elastic_operations (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
                 VALUES ('op-7','arr-a','j-bad','shrink','failed','{}','','t8')",
                [],
            )
            .is_err());
    }

    /// A node upgraded with jobs and alerts already in the table: every Elastic
    /// row gets the owner of its array, a row whose array is gone gets nobody,
    /// and a wipe whose log names a released journal leaves the shared list.
    #[test]
    fn migration_eighteen_backfills_the_owner_of_existing_array_jobs_and_alerts() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..17]).unwrap();
        conn.execute_batch(
            "INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
               VALUES ('arr-a','org-a','nas','alpha','xfs','active','','now','now');
             INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at,log) VALUES
               ('j-create','elastic_create','renamed-since','succeeded','u','now',''),
               ('j-sync','elastic_sync','alpha','succeeded','u','now',''),
               ('j-gone','elastic_destroy','gone','succeeded','u','now',''),
               ('j-scrub','pool_scrub','tank','succeeded','u','now',''),
               ('j-wipe-plain','disk_wipe','sdq','succeeded','u','now','wipefs done'),
               ('j-wipe-journal','disk_wipe','sdr','succeeded','u','now',
                 'zwolniono rezerwację dziennika macierzy Elastic gone: koniec');
             INSERT INTO nas_elastic_operations
               (operation_id,array_id,job_id,kind,state,request_json,error,created_at)
               VALUES ('op-1','arr-a','j-create','create','succeeded','{}','','now');
             INSERT INTO nas_alerts
               (alert_id,severity,subject_kind,subject_id,title,detail,raised_at,dedupe_key) VALUES
               ('a-alpha','warning','elastic-array','alpha','t','','now','k1'),
               ('a-gone','warning','elastic-array','gone','t','','now','k2'),
               ('a-disk','warning','disk','d1','t','','now','k3');",
        )
        .unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();
        let owner = |table: &str, key: &str, id: &str| -> Option<String> {
            conn.query_row(&format!("SELECT org_id FROM {table} WHERE {key} = ?1"), params![id], |r| r.get(0))
                .unwrap()
        };
        // By the operation first: the subject no longer names the array.
        assert_eq!(owner("nas_jobs", "job_id", "j-create").as_deref(), Some("org-a"));
        assert_eq!(owner("nas_jobs", "job_id", "j-sync").as_deref(), Some("org-a"));
        assert_eq!(owner("nas_jobs", "job_id", "j-gone").as_deref(), Some(""));
        assert_eq!(owner("nas_jobs", "job_id", "j-scrub"), None);
        assert_eq!(owner("nas_jobs", "job_id", "j-wipe-plain"), None);
        assert_eq!(owner("nas_jobs", "job_id", "j-wipe-journal").as_deref(), Some(""));
        assert_eq!(owner("nas_alerts", "alert_id", "a-alpha").as_deref(), Some("org-a"));
        assert_eq!(owner("nas_alerts", "alert_id", "a-gone").as_deref(), Some(""));
        assert_eq!(owner("nas_alerts", "alert_id", "a-disk"), None);
    }

    /// Names are reused: org A dissolved its `media` (the row and its
    /// operations are deleted, the jobs and the alerts stay) and org B later
    /// created its own `media`. The backfill must not hand A's older history
    /// to B through the name — only an array that existed when the row was
    /// written may claim it, and A's rows become nobody's.
    #[test]
    fn migration_eighteen_does_not_give_a_dissolved_arrays_history_to_the_org_that_reused_its_name() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..17]).unwrap();
        conn.execute_batch(
            "INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at)
               VALUES ('arr-b','org-b','nas','media','xfs','active','',
                       '2026-09-20T10:00:00Z','2026-09-20T10:00:00Z');
             INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at,log) VALUES
               ('j-a-sync','elastic_sync','media','succeeded','u-a','2026-09-10T08:00:00Z',
                 'snapraid sync of org A'),
               ('j-a-destroy','elastic_destroy','media','succeeded','u-a','2026-09-12T08:00:00Z',''),
               ('j-b-create','elastic_create','media','succeeded','u-b','2026-09-20T10:00:00Z',''),
               ('j-b-sync','elastic_sync','media','succeeded','u-b','2026-09-21T08:00:00Z','');
             INSERT INTO nas_alerts
               (alert_id,severity,subject_kind,subject_id,title,detail,raised_at,dedupe_key) VALUES
               ('a-a','warning','elastic-array','media','org A text','','2026-09-11T08:00:00Z','k-a'),
               ('a-b','warning','elastic-array','media','org B text','','2026-09-21T09:00:00Z','k-b');",
        )
        .unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();
        let owner = |table: &str, key: &str, id: &str| -> Option<String> {
            conn.query_row(&format!("SELECT org_id FROM {table} WHERE {key} = ?1"), params![id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(owner("nas_jobs", "job_id", "j-a-sync").as_deref(), Some(""), "org A's job, not B's");
        assert_eq!(owner("nas_jobs", "job_id", "j-a-destroy").as_deref(), Some(""));
        // Created in the same second as the array row: `<=`, not `<`.
        assert_eq!(owner("nas_jobs", "job_id", "j-b-create").as_deref(), Some("org-b"));
        assert_eq!(owner("nas_jobs", "job_id", "j-b-sync").as_deref(), Some("org-b"));
        assert_eq!(owner("nas_alerts", "alert_id", "a-a").as_deref(), Some(""), "org A's alert, not B's");
        assert_eq!(owner("nas_alerts", "alert_id", "a-b").as_deref(), Some("org-b"));
    }

    /// An alert still open when its array's name passes to another
    /// organisation: the next raise under the same dedupe key moves the row to
    /// the new owner, whether or not its text changed, so org A never reads
    /// org B's condition and B does not miss it.
    #[test]
    fn a_refreshed_alert_is_restamped_with_the_owner_of_the_array_that_now_holds_the_name() {
        let p = pool();
        let a = tenant_array(&p, "org-a", "media");
        let key = "elastic:media:repair";
        let raise = |detail: &str| {
            raise_alert(&p, key, "warning", "elastic-array", "media", "Macierz media wymaga naprawy", detail)
                .unwrap()
        };
        assert!(raise("the same words"));
        let mine = list_alerts_for_org(&p, "org-a", true).unwrap();
        assert_eq!(mine.len(), 1);
        let alert_id = mine[0].alert_id.clone();
        // Org A acknowledges it, and a fixed sentinel stands in for
        // `raised_at` so the reset below cannot pass by timing luck (a test
        // run within the same wall-clock second as the refresh would not
        // otherwise tell "kept" from "reset to now" apart).
        assert!(ack_alert_for_org(&p, "org-a", &alert_id).unwrap());
        p.write().unwrap().execute(
            "UPDATE nas_alerts SET raised_at = '2020-01-01T00:00:00Z' WHERE alert_id = ?1",
            params![alert_id],
        ).unwrap();
        let acked = list_alerts_for_org(&p, "org-a", true).unwrap();
        assert!(acked[0].acked_at.is_some(), "org A's ack really landed");
        assert_eq!(acked[0].raised_at, "2020-01-01T00:00:00Z");

        // Dissolved WITHOUT resolving this alert, then the name is reused.
        assert!(delete_elastic_array(&p, &a.owner, &a.array_id).unwrap());
        tenant_array(&p, "org-b", "media");

        // Identical text: still re-owned, the owner alone is a change.
        assert!(!raise("the same words"), "a refresh, not a new alert");
        assert!(list_alerts_for_org(&p, "org-a", true).unwrap().is_empty());
        let theirs = list_alerts_for_org(&p, "org-b", true).unwrap();
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].alert_id, alert_id, "the same row, re-owned, not a new one");
        // N3: a re-owned alert must not arrive pre-acked with org A's "since"
        // time — B's badge and unacked list would never show it otherwise.
        assert!(theirs[0].acked_at.is_none(), "B's condition starts unacknowledged");
        assert_ne!(theirs[0].raised_at, "2020-01-01T00:00:00Z", "B's \"since\" time is its own, not A's");
        // New text: lands on org B's row only.
        assert!(!raise("org B's disk d2"));
        assert!(list_alerts_for_org(&p, "org-a", true).unwrap().is_empty());
        let seen = list_alerts_for_org(&p, "org-b", true).unwrap();
        assert_eq!(seen.iter().map(|a| a.detail.as_str()).collect::<Vec<_>>(), vec!["org B's disk d2"]);
        // The current owner raising it again changes nothing and adds nothing,
        // and does NOT reset `raised_at` or `acked_at` again — the owner is
        // unchanged, so the CASE in the refresh must not fire a second time.
        let before_same_owner = list_alerts_for_org(&p, "org-b", true).unwrap();
        assert!(!raise("org B's disk d2"));
        let after_same_owner = list_alerts_for_org(&p, "org-b", true).unwrap();
        assert_eq!(after_same_owner[0].raised_at, before_same_owner[0].raised_at);
        assert_eq!(after_same_owner[0].acked_at, before_same_owner[0].acked_at);
        assert_eq!(list_alerts_for_org(&p, "org-b", true).unwrap().len(), 1);
        assert_eq!(list_alerts(&p, true).unwrap().len(), 1, "one row throughout");
    }

    /// The fleet badge: the published node figure counts only node-wide
    /// alerts, and a tenant's figure is exactly the list that tenant sees.
    #[test]
    fn the_alert_counts_match_what_each_tenant_is_shown() {
        let p = pool();
        tenant_array(&p, "org-a", "alpha");
        tenant_array(&p, "org-b", "bravo");
        raise_alert(&p, "elastic:alpha:x", "warning", "elastic-array", "alpha", "Macierz alpha", "").unwrap();
        raise_alert(&p, "elastic:bravo:x", "warning", "elastic-array", "bravo", "Macierz bravo", "").unwrap();
        raise_alert(&p, "elastic:bravo:y", "critical", "elastic-array", "bravo", "Macierz bravo 2", "").unwrap();
        raise_alert(&p, "disk:d1:temp", "warning", "disk", "d1", "Disk sda: hot", "").unwrap();
        raise_alert(&p, "elastic:ghost:x", "warning", "elastic-array", "ghost", "Macierz ghost", "").unwrap();
        assert_eq!(count_open_node_alerts(&p).unwrap(), 1, "only the disk");
        for (org, expected) in [("org-a", 2), ("org-b", 3), ("org-c", 1), ("", 1)] {
            assert_eq!(count_open_alerts_for_org(&p, org).unwrap(), expected, "{org}");
            assert_eq!(list_alerts_for_org(&p, org, false).unwrap().len() as u32, expected, "{org}");
        }
        let bravo = list_alerts_for_org(&p, "org-b", false).unwrap()[0].alert_id.clone();
        assert!(ack_alert_for_org(&p, "org-b", &bravo).unwrap());
        assert_eq!(count_open_alerts_for_org(&p, "org-b").unwrap(), 2, "an acknowledged alert leaves both");
    }

    #[test]
    fn the_array_names_of_an_organisation_hold_only_its_own_arrays() {
        let p = pool();
        tenant_array(&p, "org-a", "alpha");
        tenant_array(&p, "org-b", "bravo");
        assert_eq!(elastic_array_names_of_org(&p, "org-a").unwrap(),
            ["alpha"].map(String::from).into_iter().collect());
        assert!(elastic_array_names_of_org(&p, "org-c").unwrap().is_empty());
    }


    // ----- ownership of shares, targets, accounts and the access log (migration 20)

    fn owned_share(id: &str, name: &str, source_path: &str) -> ShareRow {
        ShareRow {
            share_id: id.into(),
            name: name.into(),
            protocol: "smb".into(),
            source_path: source_path.into(),
            smb: Some(NasSmbOptions::default()),
            state: "active".into(),
            created_at: now(),
            updated_at: now(),
            ..Default::default()
        }
    }

    fn owned_target(id: &str, name: &str) -> TargetRow {
        TargetRow {
            target_id: id.into(),
            name: name.into(),
            protocol: "iscsi".into(),
            wwn: format!("iqn.2026-09.pl.test:node.{name}"),
            auth_method: "none".into(),
            state: "disabled".into(),
            created_at: now(),
            updated_at: now(),
            ..Default::default()
        }
    }

    /// Org B's share is not in org A's list, not readable by id, cannot be
    /// overwritten by an upsert carrying its id (neither the row nor its
    /// grants), and cannot be deleted — every answer is the one a missing
    /// share gets. The node-wide list the smb/exports generator reads still
    /// holds both, because the node serves both.
    #[test]
    fn another_orgs_share_is_invisible_and_refused() {
        let p = pool();
        let mut theirs = owned_share("s-b", "ksiegowosc", "/mnt/tank/ksiegowosc");
        theirs.smb.as_mut().unwrap().users = vec![NasShareAccess { user: "bogna".into(), mode: "rw".into() }];
        upsert_share(&p, "org-b", &theirs).unwrap();
        upsert_share(&p, "org-a", &owned_share("s-a", "projekty", "/mnt/tank/projekty")).unwrap();

        let names = |org: &str| -> Vec<String> {
            list_shares_of_org(&p, org).unwrap().into_iter().map(|s| s.name).collect()
        };
        assert_eq!(names("org-a"), vec!["projekty".to_string()]);
        assert_eq!(names("org-b"), vec!["ksiegowosc".to_string()]);
        assert!(names("").is_empty(), "an empty org id is nobody");
        assert_eq!(list_shares(&p).unwrap().len(), 2, "the node serves both");
        assert!(share(&p, "org-a", "s-b").unwrap().is_none());
        assert!(share(&p, "org-b", "s-b").unwrap().is_some());

        // A write carrying org B's id under org A's name changes nothing.
        let mut hijack = theirs.clone();
        hijack.source_path = "/mnt/tank/elsewhere".into();
        hijack.smb.as_mut().unwrap().users = vec![NasShareAccess { user: "adam".into(), mode: "rw".into() }];
        assert!(upsert_share(&p, "org-a", &hijack).is_err());
        let kept = share(&p, "org-b", "s-b").unwrap().unwrap();
        assert_eq!(kept.source_path, "/mnt/tank/ksiegowosc");
        assert_eq!(share_grants(&p, "s-b").unwrap(), theirs.smb.as_ref().unwrap().users);

        assert!(!delete_share(&p, "org-a", "s-b").unwrap());
        assert!(share(&p, "org-b", "s-b").unwrap().is_some());
        assert_eq!(share_counts(&p, "org-a").unwrap(), (1, 0));
        assert!(upsert_share(&p, "", &owned_share("s-x", "x", "/mnt/tank/x")).is_err(), "no owner, no row");
    }

    /// The same four answers for a block target.
    #[test]
    fn another_orgs_target_is_invisible_and_refused() {
        let p = pool();
        let mut theirs = owned_target("t-b", "vm-b");
        theirs.initiators = vec!["iqn.1998-01.com.vmware:esx-b".into()];
        upsert_target(&p, "org-b", &theirs).unwrap();
        upsert_target(&p, "org-a", &owned_target("t-a", "vm-a")).unwrap();

        let names: Vec<String> = list_targets_of_org(&p, "org-a").unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["vm-a".to_string()]);
        assert_eq!(list_targets(&p).unwrap().len(), 2, "the kernel reconcile sees both");
        assert!(target(&p, "org-a", "t-b").unwrap().is_none());

        let mut hijack = theirs.clone();
        hijack.enabled = true;
        hijack.initiators = vec!["iqn.1998-01.com.vmware:esx-a".into()];
        assert!(upsert_target(&p, "org-a", &hijack).is_err());
        let kept = target(&p, "org-b", "t-b").unwrap().unwrap();
        assert!(!kept.enabled);
        assert_eq!(kept.initiators, theirs.initiators);

        assert!(!delete_target(&p, "org-a", "t-b").unwrap());
        assert!(target(&p, "org-b", "t-b").unwrap().is_some());
        assert_eq!(target_counts(&p, "org-a").unwrap(), (1, 0));
        assert_eq!(target_owners(&p).unwrap().get("t-b").map(String::as_str), Some("org-b"));
    }

    /// A share account is a node account: its NAME is taken for everybody,
    /// but only its organisation lists it, changes it or deletes it — and the
    /// shares listed under it are that organisation's only.
    #[test]
    fn another_orgs_share_account_is_neither_listed_nor_changed() {
        let p = pool();
        upsert_share_user(&p, "org-b", "bogna", "kadry").unwrap();
        upsert_share_user(&p, "org-a", "anna", "").unwrap();
        let mut theirs = owned_share("s-b", "kadry", "/mnt/tank/kadry");
        theirs.smb.as_mut().unwrap().users = vec![NasShareAccess { user: "anna".into(), mode: "ro".into() }];
        upsert_share(&p, "org-b", &theirs).unwrap();

        let a = list_share_users(&p, "org-a").unwrap();
        assert_eq!(a.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(), vec!["anna"]);
        assert!(a[0].shares.is_empty(), "org B's share name is not listed under org A's account");
        assert!(share_user_exists(&p, "org-a", "anna").unwrap());
        assert!(!share_user_exists(&p, "org-a", "bogna").unwrap());
        assert_eq!(share_user_owner(&p, "bogna").unwrap().as_deref(), Some("org-b"));

        assert!(upsert_share_user(&p, "org-a", "bogna", "przejęte").is_err());
        assert!(!delete_share_user(&p, "org-a", "bogna").unwrap());
        let b = list_share_users(&p, "org-b").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].description, "kadry");
    }

    /// The access log is scoped to the asking organisation's shares: rows,
    /// the total, the facets and the count. A line keeps the owner its share
    /// had when it was collected, so a deleted share's log stays with its
    /// organisation and another organisation's new share of the same name
    /// does not inherit it.
    #[test]
    fn the_access_log_is_scoped_to_the_callers_shares() {
        let p = pool();
        upsert_share(&p, "org-a", &owned_share("s-a", "projekty", "/mnt/tank/projekty")).unwrap();
        upsert_share(&p, "org-b", &owned_share("s-b", "kadry", "/mnt/tank/kadry")).unwrap();
        let event = |share: &str, user: &str| NasAccessEvent {
            at: now(),
            share: share.into(),
            user: user.into(),
            client: "10.0.0.9".into(),
            operation: "openat".into(),
            result: "ok".into(),
            target: "plik.txt".into(),
            detail: String::new(),
            event_id: 0,
        };
        insert_access_events(&p, &[event("projekty", "anna"), event("kadry", "bogna"), event("kadry", "bogna")])
            .unwrap();

        let (rows, total) = access_events(&p, "org-a", &AccessFilter { limit: 100, ..Default::default() }).unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows.iter().map(|e| e.share.as_str()).collect::<Vec<_>>(), vec!["projekty"]);
        // Asking for the other tenant's share by name finds nothing either.
        let (rows, total) =
            access_events(&p, "org-a", &AccessFilter { share: "kadry", limit: 100, ..Default::default() }).unwrap();
        assert!(rows.is_empty());
        assert_eq!(total, 0);
        let (shares, users, _) = access_facets(&p, "org-a").unwrap();
        assert_eq!(shares, vec!["projekty".to_string()]);
        assert_eq!(users, vec!["anna".to_string()]);
        assert_eq!(access_event_count(&p, "org-a").unwrap(), 1);
        assert_eq!(access_event_count(&p, "org-b").unwrap(), 2);
        assert_eq!(access_event_count(&p, "").unwrap(), 0);

        // Org B deletes `kadry`, org A creates its own `kadry`: B's old
        // lines stay B's.
        assert!(delete_share(&p, "org-b", "s-b").unwrap());
        upsert_share(&p, "org-a", &owned_share("s-a2", "kadry", "/mnt/tank/kadry-a")).unwrap();
        assert_eq!(access_event_count(&p, "org-a").unwrap(), 1);
        assert_eq!(access_event_count(&p, "org-b").unwrap(), 2);
        insert_access_events(&p, &[event("kadry", "anna")]).unwrap();
        assert_eq!(access_event_count(&p, "org-a").unwrap(), 2, "a NEW line follows the share's owner now");
    }

    /// A target's portal-drift alert names the target and its network, so it
    /// belongs to the target's organisation, not to every node admin.
    #[test]
    fn a_targets_alert_belongs_to_the_targets_organisation() {
        let p = pool();
        upsert_target(&p, "org-a", &owned_target("t-a", "vm-a")).unwrap();
        raise_alert(&p, "target:t-a:drift", "warning", "target", "vm-a", "Target vm-a: the portal address moved", "")
            .unwrap();
        assert_eq!(list_alerts_for_org(&p, "org-a", false).unwrap().len(), 1);
        assert!(list_alerts_for_org(&p, "org-b", false).unwrap().is_empty());
        assert_eq!(count_open_node_alerts(&p).unwrap(), 0, "not published node-wide");
    }

    /// Migration 20's owner for what it cannot attribute is the platform's
    /// default organisation, spelled as a literal in the SQL; this pins the
    /// literal to the constant so the two can never drift apart.
    #[test]
    fn the_legacy_owner_is_the_platform_default_organisation() {
        assert_eq!(LEGACY_OWNER, crate::services::org::DEFAULT_ORG_ID);
        let (version, sql) = MIGRATIONS[19];
        assert_eq!(version, 20);
        assert!(sql.contains(&format!("'{LEGACY_OWNER}'")));
    }

    /// A share on the union of an array whose own owner is EMPTY (critic
    /// wave 3, MINOR 2) would inherit '' — served by smbd, listed to nobody,
    /// manageable by nobody. It falls to the default organisation instead, and
    /// the account granted only there and the share's job follow it.
    #[test]
    fn migration_20_gives_a_share_on_an_ownerless_array_to_the_default_org() {
        let conn = Connection::open_in_memory().unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..19]).unwrap();
        conn.execute_batch(
            "INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at) VALUES
               ('arr-x','','nas','sierota','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z');
             INSERT INTO nas_shares (share_id,name,protocol,source_path,created_at,updated_at) VALUES
               ('s7','sierota','smb','/mnt/sierota','2026-09-02T00:00:00Z','2026-09-02T00:00:00Z');
             INSERT INTO nas_share_users (name,description,created_at) VALUES
               ('onlyorphan','','2026-09-02T00:00:00Z');
             INSERT INTO nas_share_grants (share_id,user,mode) VALUES ('s7','onlyorphan','rw');
             INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at) VALUES
               ('j7','share_create','sierota','ok','u','2026-09-02T00:00:00Z');",
        )
        .unwrap();

        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();

        let owner = |sql: &str, id: &str| -> Option<String> {
            conn.query_row(sql, params![id], |r| r.get(0)).unwrap()
        };
        assert_eq!(owner("SELECT org_id FROM nas_shares WHERE share_id=?1", "s7").as_deref(), Some(LEGACY_OWNER));
        assert_eq!(
            owner("SELECT org_id FROM nas_share_users WHERE name=?1", "onlyorphan").as_deref(),
            Some(LEGACY_OWNER)
        );
        assert_eq!(owner("SELECT org_id FROM nas_jobs WHERE job_id=?1", "j7").as_deref(), Some(LEGACY_OWNER));
    }

    /// Migration 20 on rows written before ownership existed: a share on an
    /// Elastic union (or a folder in it) goes to the array's organisation,
    /// everything else to the default organisation; an account follows its
    /// grants when they agree on one organisation; access lines, jobs and
    /// target alerts follow their share/target by name, and a job older than
    /// the share now holding its name does not take that share's owner.
    #[test]
    fn migration_20_backfills_owners_from_the_union_else_the_default_org() {
        let conn = Connection::open_in_memory().unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..19]).unwrap();
        conn.execute_batch(
            "INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at) VALUES
               ('arr-b','org-b','nas','media','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z');
             INSERT INTO nas_shares (share_id,name,protocol,source_path,created_at,updated_at) VALUES
               ('s-union','filmy','smb','/mnt/media','2026-09-02T00:00:00Z','2026-09-02T00:00:00Z'),
               ('s-folder','zdjecia','nfs','/mnt/media/zdjecia','2026-09-02T00:00:00Z','2026-09-02T00:00:00Z'),
               ('s-lookalike','stare','smb','/mnt/media-old','2026-09-02T00:00:00Z','2026-09-02T00:00:00Z'),
               ('s-pool','projekty','smb','/mnt/tank/projekty','2026-09-10T00:00:00Z','2026-09-10T00:00:00Z');
             INSERT INTO nas_share_users (name,description,created_at) VALUES
               ('bogna','','2026-09-02T00:00:00Z'),
               ('mixed','','2026-09-02T00:00:00Z'),
               ('idle','','2026-09-02T00:00:00Z');
             INSERT INTO nas_share_grants (share_id,user,mode) VALUES
               ('s-union','bogna','rw'),('s-union','mixed','rw'),('s-pool','mixed','ro');
             INSERT INTO nas_targets (target_id,name,protocol,wwn,created_at,updated_at) VALUES
               ('t1','vm','iscsi','iqn.2026-09.pl.test:n.vm','2026-09-02T00:00:00Z','2026-09-02T00:00:00Z');
             INSERT INTO nas_access_events (at,share,user,client,operation,result,target) VALUES
               ('2026-09-03T00:00:00Z','filmy','bogna','c','openat','ok','a'),
               ('2026-09-03T00:00:00Z','usuniety','x','c','openat','ok','b');
             INSERT INTO nas_jobs (job_id,kind,subject,status,started_by,started_at) VALUES
               ('j-union','share_create','filmy','ok','u','2026-09-02T00:00:00Z'),
               ('j-old','share_delete','projekty','ok','u','2026-09-05T00:00:00Z'),
               ('j-target','target_create','vm','ok','u','2026-09-02T00:00:01Z'),
               ('j-import','config_import','node','ok','u','2026-09-02T00:00:00Z'),
               ('j-pool','pool_scrub','tank','ok','u','2026-09-02T00:00:00Z');
             INSERT INTO nas_alerts (alert_id,severity,subject_kind,subject_id,title,detail,raised_at,dedupe_key) VALUES
               ('a-t','warning','target','vm','drift','','2026-09-03T00:00:00Z','target:t1:drift'),
               ('a-d','warning','disk','sda','hot','','2026-09-03T00:00:00Z','disk:sda:temp');",
        )
        .unwrap();

        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();

        let owner = |sql: &str, id: &str| -> Option<String> {
            conn.query_row(sql, params![id], |r| r.get(0)).unwrap()
        };
        let share_owner = |id: &str| owner("SELECT org_id FROM nas_shares WHERE share_id=?1", id);
        assert_eq!(share_owner("s-union").as_deref(), Some("org-b"));
        assert_eq!(share_owner("s-folder").as_deref(), Some("org-b"));
        assert_eq!(share_owner("s-lookalike").as_deref(), Some("org-default"), "/mnt/media-old is not in /mnt/media");
        assert_eq!(share_owner("s-pool").as_deref(), Some("org-default"));

        let user_owner = |id: &str| owner("SELECT org_id FROM nas_share_users WHERE name=?1", id);
        assert_eq!(user_owner("bogna").as_deref(), Some("org-b"));
        assert_eq!(user_owner("mixed").as_deref(), Some("org-default"), "grants in two organisations");
        assert_eq!(user_owner("idle").as_deref(), Some("org-default"), "no grants at all");

        assert_eq!(owner("SELECT org_id FROM nas_targets WHERE target_id=?1", "t1").as_deref(), Some("org-default"));

        let event_owner = |share: &str| owner("SELECT org_id FROM nas_access_events WHERE share=?1", share);
        assert_eq!(event_owner("filmy").as_deref(), Some("org-b"));
        assert_eq!(event_owner("usuniety").as_deref(), Some("org-default"));

        let job_owner = |id: &str| owner("SELECT org_id FROM nas_jobs WHERE job_id=?1", id);
        assert_eq!(job_owner("j-union").as_deref(), Some("org-b"));
        // `projekty` was created on 09-10, AFTER this job ran: whatever the
        // job was about, it was not that share.
        assert_eq!(job_owner("j-old").as_deref(), Some("org-default"));
        assert_eq!(job_owner("j-target").as_deref(), Some("org-default"));
        assert_eq!(job_owner("j-import").as_deref(), Some("org-default"));
        assert_eq!(job_owner("j-pool"), None, "a pool job stays node-wide");

        let alert_owner = |id: &str| owner("SELECT org_id FROM nas_alerts WHERE alert_id=?1", id);
        assert_eq!(alert_owner("a-t").as_deref(), Some("org-default"));
        assert_eq!(alert_owner("a-d"), None, "a disk alert stays node-wide");

        let unowned: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM nas_shares WHERE org_id='')
                      + (SELECT COUNT(*) FROM nas_share_users WHERE org_id='')
                      + (SELECT COUNT(*) FROM nas_targets WHERE org_id='')
                      + (SELECT COUNT(*) FROM nas_access_events WHERE org_id='')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unowned, 0, "every legacy row has an owner that can manage it");
    }

    // ----- migration 21: codes instead of English -----------------------------

    /// Rows raised before migration 21 are coded only where the code follows
    /// from structured columns: a parked request's alert from its request
    /// row, a disk's health alert from its severity and the disk's recorded
    /// name — as LAST KNOWN, since nothing proves the disk is still there.
    /// Everything else stays uncoded ('' / '{}' / '[]') and is shown as such;
    /// no English sentence is parsed back into a code.
    #[test]
    fn migration_21_backfills_alert_codes_only_from_structured_columns() {
        let conn = Connection::open_in_memory().unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..20]).unwrap();
        conn.execute_batch(
            "INSERT INTO nas_disks (disk_id,name,model,serial,size_bytes,kind,first_seen_at,last_seen_at,
                                    health,health_reason) VALUES
               ('wwn-a','sdq','HGST','S1',1,'hdd','2026-09-01T00:00:00Z','2026-09-22T00:00:00Z',
                'warning','8 reallocated sectors');
             INSERT INTO nas_pending_approvals (request_id,operation,subject,detail,payload_json,status,
                                                org_id,addon_id,requested_by,requested_at,expires_at) VALUES
               ('req-1','pool_destroy','tank','destroys the pool','{}','pending','org-a','nas','u',
                '2026-09-22T00:00:00Z','2026-09-23T00:00:00Z');
             INSERT INTO nas_alerts (alert_id,severity,subject_kind,subject_id,title,detail,raised_at,dedupe_key) VALUES
               ('a-disk','warning','disk','wwn-a','Disk sdq: warning','8 reallocated sectors','2026-09-22T00:00:00Z','disk:wwn-a:health'),
               ('a-gone','critical','disk','wwn-b','Disk: critical','ZFS reports this disk FAULTED','2026-09-22T00:00:00Z','disk:wwn-b:health'),
               ('a-temp','warning','disk','wwn-a','hot','','2026-09-22T00:00:00Z','disk:wwn-a:temp'),
               ('a-appr','warning','approval','req-1','a red-path operation on ''tank'' waits','x','2026-09-22T00:00:00Z','approval:req-1'),
               ('a-lost','warning','approval','req-gone','a red-path operation','x','2026-09-22T00:00:00Z','approval:req-gone'),
               ('a-arr','warning','elastic-array','media','Macierz wymaga interwencji','x','2026-09-22T00:00:00Z','elastic:m:restore');
             INSERT INTO nas_alerts (alert_id,severity,subject_kind,subject_id,title,detail,raised_at,resolved_at,dedupe_key) VALUES
               ('a-old','warning','disk','wwn-a','Disk sdy: warning','8 reallocated sectors','2026-09-01T00:00:00Z',
                '2026-09-02T00:00:00Z','disk:wwn-a:health');",
        )
        .unwrap();

        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();
        let p: DbPool = Arc::new(crate::db::Db::from_connection(conn));
        let alerts = list_alerts(&p, true).unwrap();
        let by_id = |id: &str| alerts.iter().find(|a| a.alert_id == id).unwrap_or_else(|| panic!("{id}")).clone();
        let params = |a: &NasAlert| a.params.clone().into_iter().collect::<Vec<_>>();
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());

        let disk = by_id("a-disk");
        assert_eq!(disk.code, "disk_health");
        assert_eq!(
            params(&disk),
            vec![pair("health", "warning"), pair("name", "sdq"), pair("name_source", "last_known")]
        );
        assert!(disk.reasons.is_empty(), "the English detail is not parsed back into codes");
        assert_eq!(disk.detail, "8 reallocated sectors", "the English stays as the tooltip");

        let gone = by_id("a-gone");
        assert_eq!(gone.code, "disk_health");
        assert_eq!(params(&gone), vec![pair("health", "critical"), pair("name_source", "unknown")]);

        // Wave-4 critic minor 1: a RESOLVED row of the same disk was raised
        // when the disk was called sdy. Its disk is sdq today, and naming the
        // old row sdq would be a claim nothing stored; the title is not parsed.
        let old = alerts_for_subject(&p, "disk", "wwn-a")
            .unwrap()
            .into_iter()
            .find(|a| a.alert_id == "a-old")
            .expect("the closed row");
        assert!(old.resolved_at.is_some(), "the premise: a closed row");
        assert_eq!(old.code, "disk_health");
        assert_eq!(params(&old), vec![pair("health", "warning"), pair("name_source", "unknown")]);
        assert_eq!(old.title, "Disk sdy: warning", "the English stays as the tooltip");

        let approval = by_id("a-appr");
        assert_eq!(approval.code, "approval_pending");
        assert_eq!(params(&approval), vec![pair("operation", "pool_destroy"), pair("subject", "tank")]);

        for uncoded in ["a-temp", "a-lost", "a-arr"] {
            let a = by_id(uncoded);
            assert!(a.code.is_empty() && a.params.is_empty() && a.reasons.is_empty(), "{a:?}");
            assert!(!a.title.is_empty(), "{uncoded} keeps its text for the fallback");
        }

        // And a disk row from before the migration has no codes: '[]'.
        assert!(disk_row(&p, "wwn-a").unwrap().unwrap().health_reasons.is_empty());
    }

    /// `store_smart` writes the codes beside the sentence it derives from
    /// them, and `disk_row` reads both back — what a restart seeds from.
    #[test]
    fn a_smart_read_stores_its_reason_codes_beside_the_sentence() {
        let p = pool();
        upsert_disk_seen(
            &p,
            &DiskIdentity {
                disk_id: "wwn-a",
                name: "sdq",
                model: "HGST",
                serial: "S1",
                wwn: None,
                size_bytes: 1,
                kind: "hdd",
            },
        )
        .unwrap();
        let reasons = vec![
            super::super::disks::coded_reason("temperature_over_limit", &[("celsius", "61".into()), ("limit", "55".into())]),
            super::super::disks::coded_reason("crc_errors", &[("count", "3".into())]),
        ];
        store_smart(&p, "wwn-a", "{}", "critical", &reasons).unwrap();
        let row = disk_row(&p, "wwn-a").unwrap().unwrap();
        assert_eq!(row.health, "critical");
        assert_eq!(row.health_reasons, reasons);
        assert_eq!(row.health_reason, "61°C (over the 55°C limit); 3 UDMA CRC errors (cable/backplane)");

        // A column that does not parse is no codes, not a failed read.
        p.write().unwrap().execute("UPDATE nas_disks SET health_reasons = 'nope'", []).unwrap();
        let row = disk_row(&p, "wwn-a").unwrap().unwrap();
        assert!(row.health_reasons.is_empty());
        assert_eq!(row.health, "critical");
    }

    /// The codes are part of an alert's text: an open row raised uncoded (an
    /// older build) is coded by the next raise even when its English did not
    /// change, keeping its `raised_at`; an identical raise writes nothing.
    #[test]
    fn a_raise_codes_an_open_uncoded_row_in_place_and_an_identical_one_writes_nothing() {
        let p = pool();
        assert!(raise_alert(&p, "disk:a:health", "warning", "disk", "a", "Disk sda: warning", "2 pending sectors").unwrap());
        p.write()
            .unwrap()
            .execute("UPDATE nas_alerts SET raised_at = '2026-09-01T00:00:00Z'", [])
            .unwrap();
        let text = AlertText::new("disk_health", "Disk sda: warning", "2 pending sectors")
            .param("health", "warning")
            .param("name", "sda")
            .param("name_source", "live")
            .reasons(vec![super::super::disks::coded_reason("pending_sectors", &[("count", "2".into())])]);
        assert!(!raise_coded_alert(&p, "disk:a:health", "warning", "disk", "a", &text).unwrap(), "not a new event");
        let open = list_alerts(&p, true).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].code, "disk_health");
        assert_eq!(open[0].params.get("name").map(String::as_str), Some("sda"));
        assert_eq!(open[0].reasons, text.reasons);
        assert_eq!(open[0].raised_at, "2026-09-01T00:00:00Z", "the condition began when it began");

        // Identical: the refresh matches no row (nothing changed), and the
        // insert is refused by the open-dedupe index.
        let changed = |p: &DbPool| p.read().unwrap().query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0)).unwrap();
        let before = changed(&p);
        assert!(!raise_coded_alert(&p, "disk:a:health", "warning", "disk", "a", &text).unwrap());
        assert_eq!(changed(&p), before, "an identical raise writes nothing");

        // A changed parameter alone is a refresh.
        let hotter = text.clone().param("name_source", "last_known");
        raise_coded_alert(&p, "disk:a:health", "warning", "disk", "a", &hotter).unwrap();
        assert_eq!(list_alerts(&p, true).unwrap()[0].params.get("name_source").map(String::as_str), Some("last_known"));
    }

    /// One row whose coded columns do not parse is listed uncoded — its
    /// English is still there for the fallback — instead of failing the
    /// whole list, which would blank the alert table over one row.
    #[test]
    fn an_alert_whose_codes_do_not_parse_is_listed_uncoded() {
        let p = pool();
        let text = AlertText::new("elastic_sync_held", "Sync wstrzymany", "x").param("array", "media");
        raise_coded_alert(&p, "elastic:media:repair", "warning", "elastic-array", "media", &text).unwrap();
        raise_coded_alert(&p, "disk:b:health", "warning", "disk", "b", &AlertText::new("disk_health", "Disk sdb: warning", ""))
            .unwrap();
        p.write()
            .unwrap()
            .execute("UPDATE nas_alerts SET params = '{broken' WHERE dedupe_key = 'disk:b:health'", [])
            .unwrap();
        let alerts = list_alerts(&p, true).unwrap();
        assert_eq!(alerts.len(), 2);
        let broken = alerts.iter().find(|a| a.subject_id == "b").unwrap();
        assert!(broken.code.is_empty() && broken.params.is_empty(), "{broken:?}");
        assert_eq!(broken.title, "Disk sdb: warning");
        let fine = alerts.iter().find(|a| a.subject_id == "media").unwrap();
        assert_eq!(fine.code, "elastic_sync_held");
    }

    /// Every alert TentaNas raises is coded: no production raiser in the
    /// files converted by M1 calls the uncoded `raise_alert`, and every code
    /// they raise is non-empty and documented on `NasAlert::code` — the list
    /// the screens word.
    #[test]
    fn every_alert_kind_tentanas_raises_carries_a_documented_code() {
        let protocol = include_str!("../../../tentaflow-protocol/src/tentanas.rs");
        // The doc comment of `NasAlert::code`: from the struct to the field.
        let start = protocol.find("pub struct NasAlert {").expect("NasAlert");
        let doc = &protocol[start..];
        let doc = &doc[..doc.find("pub code: String").expect("the code field")];
        let sources = [
            ("disks.rs", include_str!("disks.rs")),
            ("pools.rs", include_str!("pools.rs")),
            ("elastic.rs", include_str!("elastic.rs")),
            ("scheduler.rs", include_str!("scheduler.rs")),
            ("approvals.rs", include_str!("approvals.rs")),
            ("fleet.rs", include_str!("fleet.rs")),
            ("forward.rs", include_str!("forward.rs")),
            ("shares.rs", include_str!("shares.rs")),
            ("targets.rs", include_str!("targets.rs")),
        ];
        let mut raised = Vec::new();
        for (file, source) in sources {
            // Everything above the file's test module (`mod tests {`, or
            // elastic.rs's `pub(crate) mod tests {`); a lone `#[cfg(test)]`
            // helper above it is scanned too, which only makes this stricter.
            let production = source.split("mod tests {").next().unwrap_or(source);
            assert!(
                !production.contains("store::raise_alert(") && !production.contains(" raise_alert("),
                "{file} raises an uncoded alert"
            );
            let mut rest = production;
            while let Some(at) = rest.find("AlertText::new(") {
                rest = &rest[at + "AlertText::new(".len()..];
                let code = rest.trim_start().strip_prefix('"').and_then(|r| r.split('"').next()).unwrap_or("");
                raised.push((file, code.to_string()));
            }
        }
        let codes: std::collections::BTreeSet<&str> = raised.iter().map(|(_, c)| c.as_str()).collect();
        assert!(codes.len() >= 10, "the scan found the raisers: {raised:?}");
        for (file, code) in &raised {
            assert!(!code.is_empty(), "{file} raises an alert with an empty code");
            assert!(doc.contains(&format!("'{code}'")), "{file}: '{code}' is not documented on NasAlert::code");
        }
    }

    /// The node-wide sweep alert ('targets:reconcile') of an older build
    /// named other organisations' targets in its detail. Migration 21
    /// rewrites every such row — the RESOLVED ones too, which the disk and
    /// alert history still returns — to the neutral text the startup rewrite
    /// in targets.rs gives an open one, and codes it 'targets_sweep_stale'.
    /// Pinned to targets.rs's own constants, so the two never drift apart.
    #[test]
    fn migration_21_scrubs_every_old_node_sweep_alert_to_the_startup_rewrite_text() {
        /// A `const NAME: &str = "…";` of targets.rs, as the compiler reads
        /// it (a `\` at a line end drops the newline and the next line's
        /// leading whitespace).
        fn targets_const(name: &str) -> String {
            let source = include_str!("targets.rs");
            let head = format!("const {name}: &str = \"");
            let start = source.find(&head).unwrap_or_else(|| panic!("{name} in targets.rs")) + head.len();
            let literal = &source[start..start + source[start..].find("\";").expect("end of literal")];
            let mut parts = literal.split("\\\n");
            let mut out = parts.next().unwrap_or("").to_string();
            for part in parts {
                out.push_str(part.trim_start());
            }
            out
        }
        let conn = Connection::open_in_memory().unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, &MIGRATIONS[..20]).unwrap();
        conn.execute_batch(
            "INSERT INTO nas_alerts (alert_id,severity,subject_kind,subject_id,title,detail,raised_at,resolved_at,dedupe_key) VALUES
               ('s-old','warning','node','targets','Block targets: old wording','vm-ksiegowosc: configfs refused; vm-b: busy',
                '2026-09-01T00:00:00Z','2026-09-02T00:00:00Z','targets:reconcile'),
               ('s-open','warning','node','targets','Block targets: old wording','vm-ksiegowosc: configfs refused',
                '2026-09-03T00:00:00Z',NULL,'targets:reconcile');",
        )
        .unwrap();
        crate::addon::app_db::run_versioned_migrations(&conn, APP, MIGRATIONS).unwrap();
        let rows: Vec<(String, String, String, String, String, String)> = conn
            .prepare("SELECT alert_id, title, detail, code, params, raised_at FROM nas_alerts ORDER BY alert_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        for (id, title, detail, code, params, _) in &rows {
            assert_eq!(title, &targets_const("SWEEP_ALERT_TITLE"), "{id}");
            assert_eq!(detail, &targets_const("SWEEP_ALERT_RESTARTED"), "{id}");
            assert!(!detail.contains("vm-"), "{id}: no target name survives");
            assert_eq!(code, "targets_sweep_stale", "{id}");
            assert_eq!(params, "{}", "{id}");
        }
        assert_eq!((rows[1].0.as_str(), rows[1].5.as_str()), ("s-open", "2026-09-03T00:00:00Z"), "the open row keeps since when");
    }

    /// A refresh moves the SUBJECT with the text: the dedupe key identifies
    /// the condition (a target's alert is keyed by its id), the subject is
    /// the name it is shown and owned by — a rename must not leave the open
    /// alert naming, and owned through, the old name. The row stays the
    /// same row (a rename is not a new event).
    #[test]
    fn a_refresh_moves_the_alert_to_its_subjects_new_name() {
        let p = pool();
        let text = AlertText::new("target_portal_moved", "Target vm-a: the portal address moved", "")
            .param("target", "vm-a");
        assert!(raise_coded_alert(&p, "target:t1:drift", "warning", "target", "vm-a", &text).unwrap());
        let before = list_alerts(&p, true).unwrap();
        let renamed = text.clone().param("target", "vm-b");
        assert!(!raise_coded_alert(&p, "target:t1:drift", "warning", "target", "vm-b", &renamed).unwrap(), "not a new event");
        let after = list_alerts(&p, true).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].alert_id, before[0].alert_id);
        assert_eq!(after[0].subject_id, "vm-b");
        // Even when nothing else changed: the subject alone is a refresh.
        raise_coded_alert(&p, "target:t1:drift", "warning", "target", "vm-c", &renamed).unwrap();
        assert_eq!(list_alerts(&p, true).unwrap()[0].subject_id, "vm-c");
    }
}
