// =============================================================================
// File: tentanas/forward.rs — the alert pipeline leaving the node (plan-02
//       §5.9/§5.10): the node's alerts and, when asked for, its audited file
//       accesses, handed to an external syslog collector and/or a webhook.
//
//       WHY here and not a metrics stack: §5.9 rejects one. What an operator
//       needs from a NAS is the EVENT — "a disk went to warning", "a delete was
//       refused on projekty" — in the collector they already run, and that is
//       one UDP line or one HTTP POST, not a scrape target.
//
//       The queue is the rows themselves: `forwarded_at` on `nas_alerts` and on
//       `nas_access_events`. There is no separate outbox, because the rows are
//       already durable and already ordered, and a second table would only add
//       a way for the two to disagree. Delivery is AT LEAST ONCE on purpose —
//       the mark happens after the send, so a crash in between repeats a line
//       instead of dropping it.
// =============================================================================

use std::time::Duration;

use anyhow::{anyhow, Result};
use tentaflow_protocol::tentanas::NasForwardSettings;

use super::db::{self as store, ForwardRow};
use crate::db::DbPool;

/// The fleet-wide setting, in the instance's synced `addon_config`: where the
/// fleet sends its events is one decision, not one per node.
const SETTINGS_KEY: &str = "__nas_alert_forward";

/// Rows per pass. Bounded so a node that was offline for a day does not send
/// its whole backlog in one burst a collector would drop anyway.
const BATCH: u32 = 200;

const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// RFC 5424 facility 16 (`local0`) — the range reserved for local use, which
/// is what an application's own events are.
const SYSLOG_FACILITY: u8 = 16;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Stored {
    enabled: bool,
    syslog_target: String,
    webhook_url: String,
    include_access: bool,
}

fn stored(main_db: &DbPool, addon_id: &str) -> Option<Stored> {
    // The prefixed read strips the prefix from every key it returns, so the
    // row for the whole key comes back with an EMPTY remainder.
    crate::db::repository::list_addon_config_prefixed(main_db, addon_id, SETTINGS_KEY)
        .ok()?
        .into_iter()
        .find(|(rest, _, _)| rest.is_empty())
        .and_then(|(_, value, _)| serde_json::from_str(&value).ok())
}

/// The settings as the card shows them, with this node's backlog and the last
/// outcome — a target that is configured but unreachable has to be visible.
pub fn settings(main_db: &DbPool, nas_db: &DbPool, addon_id: &str) -> NasForwardSettings {
    let s = stored(main_db, addon_id).unwrap_or_default();
    NasForwardSettings {
        pending: store::forward_pending(nas_db, s.include_access).unwrap_or(0),
        last_sent_at: store::setting(nas_db, store::SETTING_FORWARD_SENT_AT)
            .ok()
            .flatten(),
        last_error: store::setting(nas_db, store::SETTING_FORWARD_ERROR)
            .ok()
            .flatten()
            .unwrap_or_default(),
        enabled: s.enabled,
        syslog_target: s.syslog_target,
        webhook_url: s.webhook_url,
        include_access: s.include_access,
    }
}

/// Saves the fleet setting after checking that the targets are addresses this
/// node could actually use. A misspelled target that fails silently every two
/// minutes is the failure mode this refusal exists to prevent.
pub fn set_settings(
    main_db: &DbPool,
    nas_db: &DbPool,
    addon_id: &str,
    user_id: &str,
    enabled: bool,
    syslog_target: &str,
    webhook_url: &str,
    include_access: bool,
) -> Result<NasForwardSettings> {
    let syslog_target = syslog_target.trim();
    let webhook_url = webhook_url.trim();
    if !syslog_target.is_empty() {
        validate_syslog_target(syslog_target)?;
    }
    if !webhook_url.is_empty() {
        validate_webhook_url(webhook_url)?;
    }
    if enabled && syslog_target.is_empty() && webhook_url.is_empty() {
        return Err(anyhow!(
            "forwarding is on but neither a syslog target nor a webhook is set"
        ));
    }
    let value = serde_json::to_string(&Stored {
        enabled,
        syslog_target: syslog_target.to_string(),
        webhook_url: webhook_url.to_string(),
        include_access,
    })?;
    crate::db::repository::upsert_addon_config_value(
        main_db,
        addon_id,
        SETTINGS_KEY,
        &value,
        false,
        Some(user_id),
    )?;
    // A changed target starts from a clean verdict rather than showing the
    // error of the address it replaced.
    let _ = store::set_setting(nas_db, store::SETTING_FORWARD_ERROR, "");
    Ok(settings(main_db, nas_db, addon_id))
}

/// `host:port`, with a port that fits. The host is not resolved here: a name
/// that resolves later is a valid target, and a DNS lookup is not a validation.
pub fn validate_syslog_target(target: &str) -> Result<()> {
    let Some((host, port)) = target.rsplit_once(':') else {
        return Err(anyhow!(
            "'{target}' is not a syslog target — expected host:port"
        ));
    };
    if host.is_empty() || host.len() > 253 {
        return Err(anyhow!("'{target}' has no host"));
    }
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'[' | b']'))
    {
        return Err(anyhow!("'{host}' is not a host name or address"));
    }
    match port.parse::<u16>() {
        Ok(p) if p > 0 => Ok(()),
        _ => Err(anyhow!("'{port}' is not a port")),
    }
}

/// An `http(s)://` endpoint. Nothing else: a webhook is a POST, and a scheme
/// this app cannot POST to would fail at send time instead of at save time.
pub fn validate_webhook_url(url: &str) -> Result<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(anyhow!("'{url}' is not an http(s) webhook URL"));
    }
    if url.len() > 2048 || url.contains(['\n', '\r', ' ']) {
        return Err(anyhow!("the webhook URL is not a single plain URL"));
    }
    Ok(())
}

// ----- the wire formats -------------------------------------------------------------

/// One RFC 5424 line. `severity` maps onto syslog's: a critical alert is
/// `crit` (2), a warning is `warning` (4), everything else `info` (6).
///
/// The structured-data field carries the two facts a collector filters on —
/// which node and which kind of row — so a rule can pick out "TentaNas access
/// denials from helios" without parsing the message text.
pub fn syslog_line(row: &ForwardRow, hostname: &str, node_id: &str) -> String {
    let severity = match row.severity.as_str() {
        "critical" => 2,
        "warning" => 4,
        _ => 6,
    };
    let priority = u16::from(SYSLOG_FACILITY) * 8 + severity;
    // RFC 5424 forbids a space in these fields; a subject or hostname that has
    // one would split the line, so it is replaced rather than trusted.
    let clean = |s: &str, fallback: &str| -> String {
        let s: String = s
            .chars()
            .filter(|c| !c.is_control())
            .map(|c| if c == ' ' { '_' } else { c })
            .collect();
        if s.is_empty() {
            fallback.to_string()
        } else {
            s
        }
    };
    let message = format!("{} {}", row.summary, row.detail).trim_end().to_string();
    format!(
        "<{priority}>1 {} {} tentanas - {} [tentanas@0 node=\"{}\" kind=\"{}\"] {}",
        row.at,
        clean(hostname, "-"),
        clean(&row.id, "-"),
        clean(node_id, "-"),
        row.kind,
        message.replace(['\n', '\r'], " ")
    )
}

/// The JSON one POST carries: the node that sent it and the batch of rows.
/// One document per batch rather than one per row, so a webhook receiving a
/// backlog is called once.
pub fn webhook_body(rows: &[ForwardRow], hostname: &str, node_id: &str) -> serde_json::Value {
    serde_json::json!({
        "source": "tentanas",
        "node_id": node_id,
        "hostname": hostname,
        "sent_at": store::now(),
        "events": rows
            .iter()
            .map(|r| serde_json::json!({
                "kind": r.kind,
                "id": r.id,
                "at": r.at,
                "severity": r.severity,
                "subject": r.subject,
                "summary": r.summary,
                "detail": r.detail,
            }))
            .collect::<Vec<_>>(),
    })
}

// ----- one pass ---------------------------------------------------------------------

/// Forwards one batch if forwarding is on and something is waiting. Called
/// once a minute from the schedule loop.
pub async fn forward_tick(main_db: &DbPool, nas_db: &DbPool) {
    let Some(addon_id) = crate::db::repository::get_package_instance(main_db, super::PACKAGE_ID)
        .ok()
        .flatten()
        .map(|(addon_id, _)| addon_id)
    else {
        return;
    };
    let s = stored(main_db, &addon_id).unwrap_or_default();
    if !s.enabled || (s.syslog_target.is_empty() && s.webhook_url.is_empty()) {
        return;
    }
    if let Err(e) = forward_once(nas_db, &s).await {
        let _ = store::set_setting(nas_db, store::SETTING_FORWARD_ERROR, &e.to_string());
        tracing::warn!("tentanas: alert forwarding failed: {e}");
    }
}

async fn forward_once(nas_db: &DbPool, s: &Stored) -> Result<usize> {
    let mut rows = store::unforwarded_alerts(nas_db, BATCH)?;
    if s.include_access {
        rows.extend(store::unforwarded_access_events(
            nas_db,
            BATCH.saturating_sub(rows.len() as u32).max(1),
        )?);
    }
    if rows.is_empty() {
        return Ok(0);
    }
    let hostname = hostname();
    let node_id = crate::sync::runtime::local_node_id().unwrap_or_else(|| "local".to_string());

    if !s.syslog_target.is_empty() {
        send_syslog(&s.syslog_target, &rows, &hostname, &node_id).await?;
    }
    if !s.webhook_url.is_empty() {
        send_webhook(&s.webhook_url, &rows, &hostname, &node_id).await?;
    }
    // Marked only now: both transports agreed the batch left the node.
    store::mark_forwarded(nas_db, &rows)?;
    store::set_setting(nas_db, store::SETTING_FORWARD_SENT_AT, &store::now())?;
    store::set_setting(nas_db, store::SETTING_FORWARD_ERROR, "")?;
    Ok(rows.len())
}

async fn send_syslog(
    target: &str,
    rows: &[ForwardRow],
    hostname: &str,
    node_id: &str,
) -> Result<()> {
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| anyhow!("no local UDP socket for syslog: {e}"))?;
    socket
        .connect(target)
        .await
        .map_err(|e| anyhow!("syslog target '{target}' is not reachable: {e}"))?;
    for row in rows {
        let line = syslog_line(row, hostname, node_id);
        socket
            .send(line.as_bytes())
            .await
            .map_err(|e| anyhow!("syslog send to '{target}' failed: {e}"))?;
    }
    Ok(())
}

async fn send_webhook(
    url: &str,
    rows: &[ForwardRow],
    hostname: &str,
    node_id: &str,
) -> Result<()> {
    let body = webhook_body(rows, hostname, node_id);
    let response = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
        .map_err(|e| anyhow!("HTTP client: {e}"))?
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("webhook POST failed: {e}"))?;
    if !response.status().is_success() {
        return Err(anyhow!("the webhook answered {}", response.status()));
    }
    Ok(())
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "tentanas".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tentaflow_protocol::tentanas::NasAccessEvent;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        store::migrate(&conn).expect("migrate");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    #[test]
    fn a_target_is_checked_when_it_is_saved_not_when_it_is_used() {
        for good in ["siem.local:514", "10.10.0.9:1514", "[fd00::1]:514"] {
            assert!(validate_syslog_target(good).is_ok(), "{good}");
        }
        for bad in ["siem.local", "siem.local:0", "siem.local:70000", ":514", "a b:514"] {
            assert!(validate_syslog_target(bad).is_err(), "{bad}");
        }
        assert!(validate_webhook_url("https://siem.local/hook").is_ok());
        for bad in ["ftp://x/y", "siem.local/hook", "https://x/y z"] {
            assert!(validate_webhook_url(bad).is_err(), "{bad}");
        }
    }

    fn access_row() -> ForwardRow {
        ForwardRow {
            kind: "access",
            id: "42".to_string(),
            at: "2026-09-03T12:12:04Z".to_string(),
            severity: "warning".to_string(),
            subject: "share:projekty".to_string(),
            summary: "unlinkat fail raport.xlsx on projekty by anna".to_string(),
            detail: "NT_STATUS_ACCESS_DENIED".to_string(),
        }
    }

    #[test]
    fn the_syslog_line_is_one_rfc5424_frame_a_collector_can_filter() {
        let line = syslog_line(&access_row(), "helios", "node-1");
        // local0.warning = 16*8 + 4.
        assert!(line.starts_with("<132>1 2026-09-03T12:12:04Z helios tentanas - 42 "), "{line}");
        assert!(line.contains("[tentanas@0 node=\"node-1\" kind=\"access\"]"), "{line}");
        assert!(line.ends_with("unlinkat fail raport.xlsx on projekty by anna NT_STATUS_ACCESS_DENIED"), "{line}");

        // A critical alert maps onto syslog's crit, and a multi-line detail
        // stays one frame.
        let mut alert = access_row();
        alert.kind = "alert";
        alert.severity = "critical".to_string();
        alert.detail = "2 pending sectors\nreallocated growing".to_string();
        let line = syslog_line(&alert, "helios rack 2", "node 1");
        assert!(line.starts_with("<130>1 "), "{line}");
        assert_eq!(line.lines().count(), 1, "{line}");
        // The header fields cannot carry a space.
        assert!(line.contains(" helios_rack_2 tentanas "), "{line}");
        assert!(line.contains("node=\"node_1\""), "{line}");
    }

    #[test]
    fn the_webhook_body_carries_the_whole_batch_once() {
        let rows = vec![access_row(), access_row()];
        let body = webhook_body(&rows, "helios", "node-1");
        assert_eq!(body["source"], "tentanas");
        assert_eq!(body["node_id"], "node-1");
        assert_eq!(body["events"].as_array().expect("events").len(), 2);
        assert_eq!(body["events"][0]["kind"], "access");
        assert_eq!(body["events"][0]["detail"], "NT_STATUS_ACCESS_DENIED");
    }

    /// The queue is the rows: an alert and an access event are pending until
    /// they are marked, the access half only counts when the admin asked for
    /// it, and marking is idempotent.
    #[test]
    fn only_unforwarded_rows_are_pending_and_marking_clears_them() {
        let p = db();
        store::raise_alert(&p, "disk:a:health", "warning", "disk", "a", "Disk sda: warning", "2 reallocated sectors")
            .expect("alert");
        store::insert_access_events(
            &p,
            &[NasAccessEvent {
                at: "2026-09-03T12:12:04Z".to_string(),
                share: "projekty".to_string(),
                user: "anna".to_string(),
                client: "10.10.0.24".to_string(),
                operation: "unlinkat".to_string(),
                result: "fail".to_string(),
                target: "raport.xlsx".to_string(),
                detail: "NT_STATUS_ACCESS_DENIED".to_string(),
                event_id: 0,
            }],
        )
        .expect("event");

        assert_eq!(store::forward_pending(&p, false).expect("pending"), 1);
        assert_eq!(store::forward_pending(&p, true).expect("pending"), 2);

        let alerts = store::unforwarded_alerts(&p, 10).expect("alerts");
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, "alert");
        assert_eq!(alerts[0].severity, "warning");
        assert_eq!(alerts[0].subject, "disk:a");

        let events = store::unforwarded_access_events(&p, 10).expect("events");
        assert_eq!(events.len(), 1);
        // A refused access is worth a warning to the collector.
        assert_eq!(events[0].severity, "warning");
        assert!(events[0].summary.contains("unlinkat fail raport.xlsx"));

        let mut batch = alerts;
        batch.extend(events);
        store::mark_forwarded(&p, &batch).expect("mark");
        assert_eq!(store::forward_pending(&p, true).expect("pending"), 0);
        assert!(store::unforwarded_alerts(&p, 10).expect("alerts").is_empty());
        assert!(store::unforwarded_access_events(&p, 10)
            .expect("events")
            .is_empty());
        // A replayed mark changes nothing.
        store::mark_forwarded(&p, &batch).expect("mark again");
        assert_eq!(store::forward_pending(&p, true).expect("pending"), 0);
    }

    /// The syslog transport against a REAL socket: the collector is a UDP
    /// socket on loopback, and the frames arrive as they were built.
    #[tokio::test]
    async fn the_batch_reaches_a_real_udp_collector() {
        let collector = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("collector socket");
        let target = collector.local_addr().expect("addr").to_string();
        assert!(validate_syslog_target(&target).is_ok());

        let rows = vec![access_row(), access_row()];
        send_syslog(&target, &rows, "helios", "node-1")
            .await
            .expect("send");

        let mut buf = vec![0u8; 4096];
        for _ in 0..rows.len() {
            let n = tokio::time::timeout(Duration::from_secs(5), collector.recv(&mut buf))
                .await
                .expect("no timeout")
                .expect("datagram");
            let line = String::from_utf8_lossy(&buf[..n]);
            assert_eq!(line, syslog_line(&rows[0], "helios", "node-1"));
        }
    }
}
