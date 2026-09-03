// =============================================================================
// File: tentanas/access_log.rs — the file access audit (plan-02 §5.10), i.e.
//       the "Dziennik dostępu" of the Tasks tab (n15).
//
//       An audited SMB share loads `vfs_full_audit`, so smbd writes ONE syslog
//       line per audited operation under its own identifier. This module owns
//       both ends of that: the config lines the share section carries, and the
//       collector that reads the lines back out of the journal and turns them
//       into `nas_access_events` rows with a retention.
//
//       Three decisions worth stating out loud:
//
//       - The collector reads the JOURNAL, which is where a systemd host's
//         syslog lands, through one catalog entry with a compiled-in
//         identifier. A host whose syslog goes somewhere else reports
//         `unavailable` with the reason instead of the app quietly parsing a
//         second log format it cannot attribute to a share.
//       - Position is journald's own cursor, not a timestamp: a collection
//         continues EXACTLY where the last one stopped, so no line is read
//         twice and none is skipped because it shared a second with the last.
//       - The audit is Samba's. A share that also serves SMB Direct is NOT
//         audited on the RDMA path (§5.4b: ksmbd has no audit module), and the
//         audit state says which shares are in that position so the UI can put
//         it in front of the admin rather than leaving a silent hole.
// =============================================================================

use std::time::Duration;

use anyhow::{anyhow, Result};
use tentaflow_protocol::tentanas::{NasAccessAuditState, NasAccessEvent, NasSmbOptions};
use tentanas_helper::HelperCommand;

use super::db::{self as store, ShareRow};
use crate::db::DbPool;

/// One collection pass reads at most this many journal lines. A share under
/// load produces far more than that in a day, which is what the retention
/// ceiling in `db::prune_access_events` is for.
const READ_LIMIT: u32 = 5_000;

/// How often the schedule loop collects. Once a minute would be fine for the
/// view and wasteful for the channel, so the collector runs on its own slower
/// cadence — an audit log is read to be reviewed, not watched live.
const COLLECT_EVERY: Duration = Duration::from_secs(120);

const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The operation groups the share wizard offers, and the `vfs_full_audit`
/// operation names each expands to.
///
/// Groups rather than a free list of operations because `full_audit:success =
/// all` on a busy share writes a line per read, and an admin picking from 40
/// VFS entry points is picking blind. The names are the current Samba VFS
/// spelling (`openat`, not `open`); the helper's catalog refuses anything else,
/// so a group here and the allowlist there cannot drift apart silently.
pub const OPERATION_GROUPS: &[(&str, &[&str])] = &[
    ("sessions", &["connect", "disconnect"]),
    ("reads", &["openat", "pread"]),
    ("writes", &["pwrite", "renameat", "unlinkat", "mkdirat"]),
    ("permissions", &["fchmod", "fchown", "fset_nt_acl"]),
];

/// The group ids, for the wizard and for validation.
pub fn group_ids() -> Vec<&'static str> {
    OPERATION_GROUPS.iter().map(|(id, _)| *id).collect()
}

/// The operations `groups` audit, deduplicated and in catalog order. An
/// unknown group contributes nothing — the caller validates first.
pub fn operations_of(groups: &[String]) -> Vec<&'static str> {
    OPERATION_GROUPS
        .iter()
        .filter(|(id, _)| groups.iter().any(|g| g == id))
        .flat_map(|(_, ops)| ops.iter().copied())
        .collect()
}

/// Refuses an audit configuration that would audit nothing. A toggle that is
/// on while the share section carries no `full_audit:success`/`failure` line
/// is exactly the silent hole this phase exists to close.
pub fn validate(smb: &NasSmbOptions) -> Result<()> {
    if !smb.audit {
        return Ok(());
    }
    if let Some(unknown) = smb
        .audit_groups
        .iter()
        .find(|g| !group_ids().contains(&g.as_str()))
    {
        return Err(anyhow!("'{unknown}' is not an audited operation group"));
    }
    if smb.audit_groups.is_empty() {
        return Err(anyhow!(
            "auditing is on but no operation group is selected — nothing would be audited"
        ));
    }
    if !smb.audit_success && !smb.audit_failure {
        return Err(anyhow!(
            "auditing is on but neither successful nor refused operations are audited"
        ));
    }
    Ok(())
}

/// The `vfs_full_audit` lines of one share section, as `(key, value)` pairs in
/// the order the generated file writes them. Empty when the share does not
/// audit; `full_audit` itself is added to `vfs objects` by the caller, which
/// owns the module order.
pub fn config_lines(smb: &NasSmbOptions) -> Vec<(&'static str, String)> {
    if !smb.audit {
        return Vec::new();
    }
    let ops = operations_of(&smb.audit_groups).join(" ");
    if ops.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![(
        "full_audit:prefix",
        tentanas_helper::SMB_AUDIT_PREFIX.to_string(),
    )];
    if smb.audit_success {
        lines.push(("full_audit:success", ops.clone()));
    }
    if smb.audit_failure {
        lines.push(("full_audit:failure", ops));
    }
    lines
}

/// The auditd rules document for the audited NFS exports of this node (§5.10,
/// the NFS half). Empty vector = no audited export, and the caller removes the
/// file rather than writing an empty one.
pub fn audit_rules(shares: &[ShareRow]) -> Vec<(String, String)> {
    shares
        .iter()
        .filter(|s| s.protocol == "nfs")
        .filter(|s| s.nfs.as_ref().is_some_and(|n| n.audit))
        .map(|s| (s.name.clone(), s.source_path.clone()))
        .collect()
}

// ----- parsing what smbd wrote ------------------------------------------------------

/// One collection's worth of journal output, parsed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub events: Vec<NasAccessEvent>,
    /// The cursor journald printed for the last line it returned, so the next
    /// read continues after it. `None` when the output carried none.
    pub cursor: Option<String>,
    /// Lines that carried the audit tag but not a shape this parser can read.
    /// Counted, never guessed at: a malformed line is reported, not invented.
    pub malformed: usize,
}

/// Turns `journalctl --output=short-iso --show-cursor` output into rows.
///
/// A line looks like
/// `2026-09-03T14:12:03+0200 helios smbd[1234]: smbd_audit: anna|10.0.0.4|projekty|openat|ok|dane.xlsx`
/// — the timestamp of the journal, then smbd's own message: the pinned prefix
/// (`SMB_AUDIT_PREFIX`: user, client, share), the operation, the result, and
/// whatever arguments the operation logged. Anything else is skipped: the
/// journal of smbd carries its ordinary log lines too, and those are not
/// events.
pub fn parse_journal(text: &str) -> Parsed {
    let mut out = Parsed::default();
    for raw in text.lines() {
        let line = raw.trim_end();
        // `--show-cursor` prints exactly one trailing line, and it is not an
        // event; it is where the next read starts.
        if let Some(cursor) = line.trim_start().strip_prefix("-- cursor: ") {
            out.cursor = Some(cursor.trim().to_string());
            continue;
        }
        let Some(tag_at) = line.find(tentanas_helper::SMB_AUDIT_TAG) else {
            continue;
        };
        let stamp = line[..tag_at].split_whitespace().next().unwrap_or_default();
        let Some(at) = parse_stamp(stamp) else {
            out.malformed += 1;
            continue;
        };
        let body = line[tag_at + tentanas_helper::SMB_AUDIT_TAG.len()..].trim();
        let mut fields = body.split('|');
        // user | client | share | operation | result [| args…]
        let (Some(user), Some(client), Some(share), Some(operation), Some(result)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            out.malformed += 1;
            continue;
        };
        if share.is_empty() || operation.is_empty() {
            out.malformed += 1;
            continue;
        }
        // smbd writes `fail (NT_STATUS_…)` for a refused operation, so the
        // reason travels in the same field as the verdict.
        let (verdict, detail) = match result.split_once(" (") {
            Some((v, rest)) => (v.trim(), rest.trim_end_matches(')').to_string()),
            None => (result.trim(), String::new()),
        };
        let target: Vec<&str> = fields.collect();
        out.events.push(NasAccessEvent {
            event_id: 0,
            at,
            share: share.to_string(),
            user: user.to_string(),
            client: client.to_string(),
            operation: operation.to_string(),
            result: if verdict == "ok" { "ok" } else { "fail" }.to_string(),
            target: target.join("|"),
            detail,
        });
    }
    out
}

/// `short-iso` prints local time with an offset (`2026-09-03T14:12:03+0200`).
/// Stored as UTC, like every other timestamp in this database.
fn parse_stamp(text: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(text)
        .or_else(|_| chrono::DateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%z"))
        .ok()
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
}

// ----- the collector ----------------------------------------------------------------

/// Collects if it is time to and some share actually audits. Called once a
/// minute from the schedule loop, so the cadence lives here rather than in the
/// loop that has no opinion about audit logs.
pub async fn collect_tick(db: &DbPool) {
    let last = store::setting(db, store::SETTING_AUDIT_COLLECTED_AT)
        .ok()
        .flatten()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok());
    if let Some(last) = last {
        let age = chrono::Utc::now().signed_duration_since(last.with_timezone(&chrono::Utc));
        if age.to_std().is_ok_and(|a| a < COLLECT_EVERY) {
            return;
        }
    }
    if !audited_shares(db).is_empty() {
        if let Err(e) = collect(db, None).await {
            tracing::warn!("tentanas: access audit not collected: {e}");
        }
    }
}

/// Names of the SMB shares with auditing on, from the desired state.
pub fn audited_shares(db: &DbPool) -> Vec<String> {
    store::list_shares(db)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.protocol == "smb")
        .filter(|s| s.smb.as_ref().is_some_and(|o| o.audit))
        .map(|s| s.name)
        .collect()
}

/// One collection: read from the stored cursor, keep the lines that belong to
/// a share this node audits, store them, advance the cursor, prune. Returns
/// how many rows were appended.
///
/// The cursor is advanced ONLY after the rows are committed, so a crash in
/// between re-reads a window instead of losing it. The re-read can duplicate
/// rows; a duplicated audit line is a nuisance, a missing one is a hole, and
/// §5.10 is about not having holes.
pub async fn collect(
    db: &DbPool,
    explicit: Option<&crate::profiling::collectors::elevation::ElevationToken>,
) -> Result<usize> {
    let cursor = store::setting(db, store::SETTING_AUDIT_CURSOR)?.unwrap_or_default();
    let command = HelperCommand::SmbAuditRead {
        cursor,
        limit: READ_LIMIT,
    };
    let outcome = super::broker::run_privileged(db, &command, explicit, READ_TIMEOUT).await;
    let (out, _) = match outcome {
        Ok(pair) => pair,
        Err(e) => {
            record_state(db, "unavailable", &e.to_string());
            return Err(anyhow!("{e}"));
        }
    };
    if !out.success() {
        let detail = out
            .stderr
            .trim()
            .lines()
            .next()
            .unwrap_or("journalctl failed")
            .to_string();
        record_state(db, "unavailable", &detail);
        return Err(anyhow!("{detail}"));
    }
    let parsed = parse_journal(&out.stdout);
    let audited = audited_shares(db);
    // A share whose audit was switched off keeps whatever it already logged in
    // the table, but its NEW lines are not collected — the toggle is what the
    // admin expects to stop the log growing.
    let keep: Vec<NasAccessEvent> = parsed
        .events
        .into_iter()
        .filter(|e| audited.iter().any(|s| *s == e.share))
        .collect();
    let inserted = store::insert_access_events(db, &keep)?;
    if let Some(cursor) = parsed.cursor {
        store::set_setting(db, store::SETTING_AUDIT_CURSOR, &cursor)?;
    }
    store::prune_access_events(db)?;
    let detail = if parsed.malformed > 0 {
        format!(
            "{} audit lines were not in the expected shape and were skipped",
            parsed.malformed
        )
    } else {
        String::new()
    };
    record_state(db, "ok", &detail);
    Ok(inserted)
}

fn record_state(db: &DbPool, state: &str, detail: &str) {
    let _ = store::set_setting(db, store::SETTING_AUDIT_STATE, state);
    let _ = store::set_setting(db, store::SETTING_AUDIT_DETAIL, detail);
    let _ = store::set_setting(db, store::SETTING_AUDIT_COLLECTED_AT, &store::now());
}

/// What the view shows above the table: which shares audit, which of them lose
/// the audit on their RDMA path, how much history is kept and whether the last
/// collection worked.
pub fn state(db: &DbPool) -> NasAccessAuditState {
    let shares = store::list_shares(db).unwrap_or_default();
    let audited_shares: Vec<String> = shares
        .iter()
        .filter(|s| s.protocol == "smb")
        .filter(|s| s.smb.as_ref().is_some_and(|o| o.audit))
        .map(|s| s.name.clone())
        .collect();
    let unaudited_smb_direct: Vec<String> = shares
        .iter()
        .filter(|s| {
            s.smb
                .as_ref()
                .is_some_and(|o| o.audit && o.smb_direct)
        })
        .map(|s| s.name.clone())
        .collect();
    let audited_exports: Vec<String> = audit_rules(&shares).into_iter().map(|(n, _)| n).collect();
    NasAccessAuditState {
        audited_shares,
        audited_exports,
        unaudited_smb_direct,
        retention_days: store::ACCESS_LOG_DAYS,
        collector_state: store::setting(db, store::SETTING_AUDIT_STATE)
            .ok()
            .flatten()
            .unwrap_or_else(|| "ok".to_string()),
        detail: store::setting(db, store::SETTING_AUDIT_DETAIL)
            .ok()
            .flatten()
            .unwrap_or_default(),
        collected_at: store::setting(db, store::SETTING_AUDIT_COLLECTED_AT)
            .ok()
            .flatten(),
        event_count: store::access_event_count(db).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        store::migrate(&conn).expect("migrate");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn audited(groups: &[&str], success: bool, failure: bool) -> NasSmbOptions {
        NasSmbOptions {
            audit: true,
            audit_groups: groups.iter().map(|g| g.to_string()).collect(),
            audit_success: success,
            audit_failure: failure,
            ..Default::default()
        }
    }

    /// The generated lines are what the helper's allowlist accepts — the two
    /// ends of the channel agree on the vocabulary, checked here rather than
    /// discovered when a share refuses to apply.
    #[test]
    fn the_generated_audit_lines_pass_the_channels_own_validation() {
        let smb = audited(&["writes", "permissions"], true, true);
        let lines = config_lines(&smb);
        let mut section = String::from("[projekty]\n\tvfs objects = full_audit\n");
        for (key, value) in &lines {
            section.push_str(&format!("\t{key} = {value}\n"));
        }
        assert!(
            tentanas_helper::validate_smb_config(&section).is_ok(),
            "{section}"
        );
        // Every group's operations reach the file, in catalog order.
        let (_, success) = lines
            .iter()
            .find(|(k, _)| *k == "full_audit:success")
            .expect("success line");
        assert_eq!(success, "pwrite renameat unlinkat mkdirat fchmod fchown fset_nt_acl");
        assert_eq!(
            lines
                .iter()
                .find(|(k, _)| *k == "full_audit:prefix")
                .map(|(_, v)| v.as_str()),
            Some(tentanas_helper::SMB_AUDIT_PREFIX)
        );

        // Only failures: no success line at all.
        let only_failures = config_lines(&audited(&["reads"], false, true));
        assert!(!only_failures.iter().any(|(k, _)| *k == "full_audit:success"));
        assert_eq!(
            only_failures
                .iter()
                .find(|(k, _)| *k == "full_audit:failure")
                .map(|(_, v)| v.as_str()),
            Some("openat pread")
        );

        // A share that does not audit carries no audit line.
        assert!(config_lines(&NasSmbOptions::default()).is_empty());
    }

    #[test]
    fn an_audit_that_would_audit_nothing_is_refused() {
        assert!(validate(&NasSmbOptions::default()).is_ok());
        assert!(validate(&audited(&["reads"], true, false)).is_ok());
        // On, but no group.
        assert!(validate(&audited(&[], true, true)).is_err());
        // On, with a group, but neither result.
        assert!(validate(&audited(&["reads"], false, false)).is_err());
        // A group this node does not know.
        assert!(validate(&audited(&["everything"], true, true)).is_err());
    }

    /// Realistic journal output: two audited operations, a failure with its
    /// NT status, an ordinary smbd log line that is not an event, a line whose
    /// audit body is truncated, and the cursor `--show-cursor` appends.
    const JOURNAL: &str = "\
2026-09-03T14:12:03+0200 helios smbd[1234]: smbd_audit: anna|10.10.0.24|projekty|openat|ok|dane.xlsx
2026-09-03T14:12:04+0200 helios smbd[1234]: smbd_audit: anna|10.10.0.24|projekty|unlinkat|fail (NT_STATUS_ACCESS_DENIED)|raport.xlsx
2026-09-03T14:12:05+0200 helios smbd[1234]: Failed to open /var/lib/samba/x: Permission denied
2026-09-03T14:12:06+0200 helios smbd[1234]: smbd_audit: jan|10.10.0.31
2026-09-03T14:12:07+0200 helios smbd[1234]: smbd_audit: jan|10.10.0.31|archiwum|renameat|ok|stary.doc|nowy.doc
-- cursor: s=8c1e4a;i=1f4;b=3d2;m=1a2b3c;t=5e6f;x=9a8b
";

    #[test]
    fn realistic_audit_lines_become_rows_and_a_malformed_one_is_counted() {
        let parsed = parse_journal(JOURNAL);
        assert_eq!(parsed.malformed, 1, "the truncated audit line");
        assert_eq!(
            parsed.cursor.as_deref(),
            Some("s=8c1e4a;i=1f4;b=3d2;m=1a2b3c;t=5e6f;x=9a8b")
        );
        assert_eq!(parsed.events.len(), 3);

        let open = &parsed.events[0];
        // The journal's local time is stored as UTC.
        assert_eq!(open.at, "2026-09-03T12:12:03Z");
        assert_eq!(open.share, "projekty");
        assert_eq!(open.user, "anna");
        assert_eq!(open.client, "10.10.0.24");
        assert_eq!(open.operation, "openat");
        assert_eq!(open.result, "ok");
        assert_eq!(open.target, "dane.xlsx");
        assert!(open.detail.is_empty());

        let denied = &parsed.events[1];
        assert_eq!(denied.result, "fail");
        assert_eq!(denied.detail, "NT_STATUS_ACCESS_DENIED");
        assert_eq!(denied.target, "raport.xlsx");

        // A rename logs both names; both are kept.
        assert_eq!(parsed.events[2].target, "stary.doc|nowy.doc");

        // The smbd line that is not an audit line produced nothing at all.
        assert!(!parsed
            .events
            .iter()
            .any(|e| e.operation.contains("Permission")));
    }

    #[test]
    fn the_log_filters_by_share_user_operation_and_result() {
        let p = db();
        let rows = parse_journal(JOURNAL).events;
        assert_eq!(store::insert_access_events(&p, &rows).expect("insert"), 3);

        let all = store::access_events(&p, &store::AccessFilter::default()).expect("all");
        assert_eq!(all.1, 3);
        // Newest first.
        assert_eq!(all.0[0].operation, "renameat");

        let by_share = store::access_events(
            &p,
            &store::AccessFilter {
                share: "projekty",
                ..Default::default()
            },
        )
        .expect("share");
        assert_eq!(by_share.1, 2);
        assert!(by_share.0.iter().all(|e| e.share == "projekty"));

        let failures = store::access_events(
            &p,
            &store::AccessFilter {
                result: "fail",
                ..Default::default()
            },
        )
        .expect("result");
        assert_eq!(failures.1, 1);
        assert_eq!(failures.0[0].operation, "unlinkat");

        let by_user = store::access_events(
            &p,
            &store::AccessFilter {
                user: "jan",
                ..Default::default()
            },
        )
        .expect("user");
        assert_eq!(by_user.1, 1);

        let by_op = store::access_events(
            &p,
            &store::AccessFilter {
                operation: "openat",
                ..Default::default()
            },
        )
        .expect("operation");
        assert_eq!(by_op.1, 1);

        // Two filters at once narrow rather than widen.
        let both = store::access_events(
            &p,
            &store::AccessFilter {
                share: "projekty",
                result: "ok",
                ..Default::default()
            },
        )
        .expect("both");
        assert_eq!(both.1, 1);

        // `since` cuts the window; `limit` cuts the page but not the total.
        let since = store::access_events(
            &p,
            &store::AccessFilter {
                since: "2026-09-03T12:12:05Z",
                ..Default::default()
            },
        )
        .expect("since");
        assert_eq!(since.1, 1);
        let paged = store::access_events(
            &p,
            &store::AccessFilter {
                limit: 2,
                ..Default::default()
            },
        )
        .expect("limit");
        assert_eq!((paged.0.len(), paged.1), (2, 3));

        // The filters offer what the node actually logged.
        let (shares, users, operations) = store::access_facets(&p).expect("facets");
        assert_eq!(shares, vec!["archiwum", "projekty"]);
        assert_eq!(users, vec!["anna", "jan"]);
        assert_eq!(operations, vec!["openat", "renameat", "unlinkat"]);
    }

    #[test]
    fn retention_drops_rows_past_the_window_and_keeps_the_rest() {
        let p = db();
        let old = chrono::Utc::now() - chrono::Duration::days(i64::from(store::ACCESS_LOG_DAYS) + 1);
        let fresh = chrono::Utc::now() - chrono::Duration::hours(1);
        let stamp = |t: chrono::DateTime<chrono::Utc>| {
            t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        let event = |at: String| NasAccessEvent {
            at,
            share: "projekty".to_string(),
            user: "anna".to_string(),
            operation: "openat".to_string(),
            result: "ok".to_string(),
            ..Default::default()
        };
        store::insert_access_events(&p, &[event(stamp(old)), event(stamp(fresh))])
            .expect("insert");
        assert_eq!(store::access_event_count(&p).expect("count"), 2);
        assert_eq!(store::prune_access_events(&p).expect("prune"), 1);
        let rows = store::access_events(&p, &store::AccessFilter::default()).expect("rows");
        assert_eq!(rows.1, 1);
        assert_eq!(rows.0[0].at, stamp(fresh));
        // A second pass has nothing left to drop.
        assert_eq!(store::prune_access_events(&p).expect("prune"), 0);
    }

    /// The audit state is read from the shares, so a share that audits AND
    /// serves SMB Direct is listed as one whose RDMA path is not audited.
    #[test]
    fn the_state_names_the_shares_whose_rdma_path_is_not_audited() {
        let p = db();
        for (name, smb_direct) in [("projekty", true), ("archiwum", false)] {
            store::upsert_share(
                &p,
                &ShareRow {
                    share_id: format!("s-{name}"),
                    name: name.to_string(),
                    protocol: "smb".to_string(),
                    source_path: format!("/mnt/tank/{name}"),
                    smb: Some(NasSmbOptions {
                        smb_direct,
                        ..audited(&["writes"], true, true)
                    }),
                    ..Default::default()
                },
            )
            .expect("share");
        }
        store::upsert_share(
            &p,
            &ShareRow {
                share_id: "s-backups".to_string(),
                name: "backups".to_string(),
                protocol: "nfs".to_string(),
                source_path: "/mnt/tank/backups".to_string(),
                nfs: Some(tentaflow_protocol::tentanas::NasNfsOptions {
                    audit: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .expect("share");

        let state = state(&p);
        assert_eq!(state.audited_shares, vec!["archiwum", "projekty"]);
        assert_eq!(state.unaudited_smb_direct, vec!["projekty"]);
        assert_eq!(state.audited_exports, vec!["backups"]);
        assert_eq!(state.retention_days, store::ACCESS_LOG_DAYS);
        assert_eq!(state.event_count, 0);
        assert_eq!(audited_shares(&p), vec!["archiwum", "projekty"]);

        // The auditd document watches exactly the audited export's path.
        let rules = tentanas_helper::audit_rules_file(&audit_rules(
            &store::list_shares(&p).expect("shares"),
        ));
        assert!(rules.contains("-w /mnt/tank/backups -p rwa -k tentanas-backups"));
        assert!(!rules.contains("projekty"), "an SMB share is not an export");
        assert!(tentanas_helper::validate_audit_rules(&rules).is_ok());
    }
}
