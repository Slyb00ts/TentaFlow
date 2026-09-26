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
//
//       ONE TARGET PER ORGANISATION (wave 9b). `tentanas.db` is one per
//       node and holds every tenant's alerts and access lines, so a target
//       one organisation's admin points at its own collector may receive
//       exactly what that organisation may READ — the rule of the screens
//       (`db::VISIBLE_TO_ORG_SQL`): its own alerts, the node-wide ones every
//       organisation sees (disks, pools), and — when it asks for them — the
//       access lines of its OWN shares. Never another organisation's row,
//       never a row whose owner is gone ('' — the sole-organisation reading
//       of those is a screen rule, not extended to a collector outside the
//       product). Each organisation sets its target once for the fleet (the
//       synced `addon_config`), and every node sends it that organisation's
//       view of the node.
//
//       THE NODE-WIDE TARGET STAYS what the single setting was before:
//       node-wide alerts only, never an access line. It was NOT migrated to
//       an organisation: any organisation's admin could have set it, so
//       handing it the default organisation's alerts and access lines could
//       send one tenant's rows to a collector another tenant chose. Kept as
//       it was, it sends exactly what it sent before and nothing more; its
//       queue continues where the old one stood (migration 27).
//
//       THE QUEUE IS A CURSOR PER TARGET over the rows' own monotonic keys
//       (`db::ForwardCursor`), because one node-wide alert now goes to every
//       organisation's target and a single `forwarded_at` mark cannot say
//       which of them it reached. Delivery stays AT LEAST ONCE: a cursor
//       moves after the send. A target sends what is raised while it is on.
// =============================================================================

use std::time::Duration;

use anyhow::{anyhow, Result};
use tentaflow_protocol::tentanas::NasForwardSettings;

use super::db::{self as store, ForwardRow};
use crate::db::DbPool;

/// The node-wide target, in the instance's synced `addon_config` — the one
/// setting there was before targets were per organisation (module header).
const SETTINGS_KEY: &str = "__nas_alert_forward";

/// One organisation's target: `__nas_alert_forward/org/<org_id>`, synced like
/// the node-wide one, so an organisation sets it once for the whole fleet.
const ORG_KEY_PREFIX: &str = "__nas_alert_forward/org/";

/// Rows per pass. Bounded so a node that was offline for a day does not send
/// its whole backlog in one burst a collector would drop anyway.
const BATCH: u32 = 200;

const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// RFC 5424 facility 16 (`local0`) — the range reserved for local use, which
/// is what an application's own events are.
const SYSLOG_FACILITY: u8 = 16;

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct Stored {
    enabled: bool,
    syslog_target: String,
    webhook_url: String,
    include_access: bool,
    /// When the target was last switched on (RFC 3339): what was raised
    /// before it is not sent. Empty on the node-wide setting written before
    /// wave 9b, whose queue migration 27 carried over.
    #[serde(default)]
    enabled_at: String,
    /// When the access lines were last asked for (RFC 3339): a target that
    /// turns them on later receives the lines collected from then on, never
    /// the whole retained log (critic wave 9b, MINOR 1).
    #[serde(default)]
    access_enabled_at: String,
}

impl Stored {
    fn sends(&self) -> bool {
        self.enabled && !(self.syslog_target.is_empty() && self.webhook_url.is_empty())
    }
}

/// Which target a request or a pass is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// The node-wide target: node-wide alerts only.
    Node,
    /// One organisation's own target.
    Org(&'a str),
}

impl Target<'_> {
    fn cursor_key(&self) -> &str {
        match self {
            Self::Node => store::FORWARD_NODE_TARGET,
            Self::Org(org) => org,
        }
    }

    fn config_key(&self) -> String {
        match self {
            Self::Node => SETTINGS_KEY.to_string(),
            Self::Org(org) => format!("{ORG_KEY_PREFIX}{org}"),
        }
    }
}

/// Every stored target of the instance: the node-wide one (if ever set) and
/// every organisation's, keyed by organisation id.
fn stored_all(main_db: &DbPool, addon_id: &str) -> (Option<Stored>, Vec<(String, Stored)>) {
    // The prefixed read strips the prefix from every key it returns, so the
    // row for the whole key comes back with an EMPTY remainder.
    let rows = crate::db::repository::list_addon_config_prefixed(main_db, addon_id, SETTINGS_KEY)
        .unwrap_or_default();
    let org_rest = ORG_KEY_PREFIX.strip_prefix(SETTINGS_KEY).unwrap_or(ORG_KEY_PREFIX);
    let mut node = None;
    let mut orgs = Vec::new();
    for (rest, value, _) in rows {
        let Ok(parsed) = serde_json::from_str::<Stored>(&value) else {
            continue;
        };
        if rest.is_empty() {
            node = Some(parsed);
        } else if let Some(org) = rest.strip_prefix(org_rest).filter(|org| !org.is_empty()) {
            orgs.push((org.to_string(), parsed));
        }
    }
    (node, orgs)
}

fn stored(main_db: &DbPool, addon_id: &str, target: Target<'_>) -> Option<Stored> {
    let (node, orgs) = stored_all(main_db, addon_id);
    match target {
        Target::Node => node,
        Target::Org(org) => orgs.into_iter().find(|(id, _)| id == org).map(|(_, s)| s),
    }
}

/// Who reads a target's settings. A webhook URL is a bearer secret (a Slack
/// or SIEM ingest URL authorises whoever holds it), so nobody gets it back
/// whole: an organisation's admin sees it masked (scheme and host), anyone
/// else sees neither address — only whether forwarding is on and how it
/// fares (critic wave 9b, MAJOR 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viewer {
    Admin,
    Reader,
}

/// `https://siem.example.com/…` — the scheme and the host of a webhook URL,
/// never its path, query or credentials. An unparseable URL masks to `…`.
pub fn mask_webhook(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    match reqwest::Url::parse(url) {
        Ok(u) => match u.host_str() {
            Some(host) => match u.port() {
                Some(port) => format!("{}://{host}:{port}/…", u.scheme()),
                None => format!("{}://{host}/…", u.scheme()),
            },
            None => "…".to_string(),
        },
        Err(_) => "…".to_string(),
    }
}

/// The settings of one target as the card shows them, with this node's
/// backlog for it and its last outcome — a target that is configured but
/// unreachable has to be visible. See `Viewer` for what each reader gets.
pub fn settings(
    main_db: &DbPool,
    nas_db: &DbPool,
    addon_id: &str,
    target: Target<'_>,
    viewer: Viewer,
) -> NasForwardSettings {
    let s = stored(main_db, addon_id, target).unwrap_or_default();
    let include_access = s.include_access && target != Target::Node;
    let cursor = store::forward_cursor(nas_db, target.cursor_key()).ok().flatten();
    // A target that is on but has no cursor for its switch-on yet has sent
    // nothing on this node: the next pass places the cursor at that moment.
    let current = cursor.filter(|c| c.enabled_at == s.enabled_at);
    let pending = match (&current, s.sends()) {
        (Some(c), true) => {
            // The access half is counted from where the next pass will put
            // it, not from where it stood while the lines were off.
            let mut c = c.clone();
            if include_access && c.access_enabled_at != s.access_enabled_at {
                c.access_id = store::access_floor(nas_db, &s.access_enabled_at).unwrap_or(c.access_id);
            }
            store::forward_pending(nas_db, target.cursor_key(), include_access, &c).unwrap_or(0)
        }
        _ => 0,
    };
    let (syslog_target, webhook_url) = match viewer {
        Viewer::Admin => (s.syslog_target.clone(), mask_webhook(&s.webhook_url)),
        Viewer::Reader => (String::new(), String::new()),
    };
    // A stored webhook this node would not send to (http:// from before the
    // rule, or an entry since removed from the list): said, not hidden.
    let webhook_needs_migration = viewer == Viewer::Admin
        && !s.webhook_url.is_empty()
        && validate_webhook_url(&s.webhook_url, &AddressPolicy::load(main_db)).is_err();
    NasForwardSettings {
        pending,
        last_sent_at: current.as_ref().and_then(|c| c.last_sent_at.clone()),
        last_error: current.map(|c| neutral_error(&c.last_error)).unwrap_or_default(),
        enabled: s.enabled,
        syslog_target,
        webhook_url,
        include_access,
        webhook_needs_migration,
    }
}

/// The node-wide target is RETIRED (owner decision 2026-09-26, wave 9b): it
/// keeps sending node-wide alerts as it did, it can be deleted, and nothing
/// can edit it or create a new one. Every organisation gets the node-wide
/// alerts on its own target.
pub const FORWARD_NODE_RETIRED: &str = "refusal:forward_node_retired";

/// Deletes the retired node-wide target (see `FORWARD_NODE_RETIRED`). Its
/// cursor goes with it.
pub fn delete_node_target(main_db: &DbPool, nas_db: &DbPool, addon_id: &str) -> Result<()> {
    crate::db::repository::delete_addon_config_value(main_db, addon_id, SETTINGS_KEY)?;
    store::delete_forward_cursor(nas_db, store::FORWARD_NODE_TARGET)?;
    Ok(())
}

/// Saves an organisation's own target after checking that its addresses are
/// ones this node may send to. A misspelled target that fails silently every
/// minute is the failure mode this refusal exists to prevent.
///
/// `webhook_url` equal to the masked form of the stored one (what the
/// dialog was shown) keeps the stored URL: the secret never travels back to
/// the screen, so the screen cannot send it back either.
#[allow(clippy::too_many_arguments)]
pub fn set_settings(
    main_db: &DbPool,
    nas_db: &DbPool,
    addon_id: &str,
    user_id: &str,
    org_id: &str,
    enabled: bool,
    syslog_target: &str,
    webhook_url: &str,
    include_access: bool,
) -> Result<NasForwardSettings> {
    if org_id.is_empty() {
        return Err(anyhow!("a forwarding target needs an organisation"));
    }
    let target = Target::Org(org_id);
    let previous = stored(main_db, addon_id, target).unwrap_or_default();
    let syslog_target = syslog_target.trim();
    let webhook_url = webhook_url.trim();
    let policy = AddressPolicy::load(main_db);
    // The mask sent back keeps the stored URL — and is not judged again: a
    // webhook stored before the https rule is shown as needing a change
    // (`webhook_needs_migration`) and is not used, but it must not block
    // switching the target off or changing its syslog address (critic R3).
    let kept = !webhook_url.is_empty() && webhook_url == mask_webhook(&previous.webhook_url);
    let webhook_url = if kept { previous.webhook_url.clone() } else { webhook_url.to_string() };
    if !syslog_target.is_empty() {
        validate_syslog_target(syslog_target, &policy)?;
    }
    if !webhook_url.is_empty() && !kept {
        validate_webhook_url(&webhook_url, &policy)?;
    }
    if enabled && syslog_target.is_empty() && webhook_url.is_empty() {
        return Err(anyhow!(
            "forwarding is on but neither a syslog target nor a webhook is set"
        ));
    }
    let now = store::now();
    // Switching on starts a new stretch; saving a target that is already on
    // (a new address, the access switch) continues the one it is in.
    let enabled_at = if enabled && !previous.enabled { now.clone() } else { previous.enabled_at.clone() };
    // The access lines likewise start when they are asked for.
    let access_on = enabled && include_access;
    let access_was_on = previous.enabled && previous.include_access;
    let access_enabled_at = if access_on && !access_was_on { now } else { previous.access_enabled_at.clone() };
    let value = serde_json::to_string(&Stored {
        enabled,
        syslog_target: syslog_target.to_string(),
        webhook_url,
        include_access,
        enabled_at,
        access_enabled_at,
    })?;
    crate::db::repository::upsert_addon_config_value(
        main_db,
        addon_id,
        &target.config_key(),
        &value,
        false,
        Some(user_id),
    )?;
    // A changed target starts from a clean verdict rather than showing the
    // error of the address it replaced.
    let _ = store::record_forward_error(nas_db, target.cursor_key(), "");
    Ok(settings(main_db, nas_db, addon_id, target, Viewer::Admin))
}

// ----- where a target may point ---------------------------------------------------
//
// A target is set by one organisation's admin and served by EVERY node of the
// fleet, so without a policy it is a probe into the operator's networks: a
// POST to `127.0.0.1:<port>`, to `169.254.169.254`, to the node's LAN,
// repeated every minute from every node, with the answer written back to the
// tenant's card (critic wave 9b, MAJOR 4). So:
// - a webhook is `https://` only;
// - a target's host is resolved at send time and EVERY address it resolves
//   to must be public — no loopback, link-local, private or ULA, CGNAT,
//   multicast, broadcast or unspecified address — and the connection is
//   pinned to the address that was checked, so a second resolution cannot
//   swap in another (DNS rebinding);
// - redirects are not followed and no proxy is used (a proxy would connect
//   on our behalf to wherever it is told);
// - what the card shows of a failed send is only "not accepted" or the HTTP
//   status of a public endpoint that answered — never a socket error, an
//   address or a port.

/// Whether a target may be sent to at this address (see above).
pub fn address_allowed(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                // benchmarking 198.18/15, the retired 6to4 relay anycast,
                // and the three documentation nets (wave-9b critic, R2)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || (o[0] == 192 && o[1] == 88 && o[2] == 99)
                || (o[0] == 192 && o[1] == 0 && o[2] == 2)
                || (o[0] == 198 && o[1] == 51 && o[2] == 100)
                || (o[0] == 203 && o[1] == 0 && o[2] == 113)
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return address_allowed(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xffc0) == 0xfe80
                || (seg[0] & 0xffc0) == 0xfec0
                || (seg[0] & 0xfe00) == 0xfc00
                || (seg[0] == 0x64 && seg[1] == 0xff9b)
                || (seg[0] == 0x2001 && seg[1] == 0x0db8))
        }
    }
}

/// The platform admin's list of INTERNAL collectors an organisation's target
/// may point at (owner decision 2026-09-26, wave 9b round 3): a host name
/// (`siem.lan`), an address (`10.0.5.20`) or a network (`10.0.5.0/24`),
/// each with whether plain `http://` is allowed to it. A replicated platform
/// setting (`repository::SHARED_SETTING_KEYS`), edited only through the
/// platform admin's settings (`SettingsUpdateRequest`, Admin policy) — never
/// by an organisation.
pub const ALLOWLIST_SETTING: &str = "tentanas.forward_allowlist";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AllowEntry {
    /// `siem.lan`, `10.0.5.20`, `fd12::5`, `10.0.5.0/24`.
    pub entry: String,
    #[serde(default)]
    pub allow_http: bool,
    /// When set, the entry opens only this port (a target on another port
    /// of the same host or network is refused).
    #[serde(default)]
    pub port: Option<u16>,
}

enum AllowMatch {
    Host(String),
    Net(std::net::IpAddr, u8),
}

fn allow_match(entry: &str) -> Option<AllowMatch> {
    let entry = entry.trim().to_ascii_lowercase();
    if let Some((ip, bits)) = entry.split_once('/') {
        let ip: std::net::IpAddr = ip.parse().ok()?;
        let bits: u8 = bits.parse().ok()?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        return (bits <= max).then_some(AllowMatch::Net(ip, bits));
    }
    if let Ok(ip) = entry.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        return Some(AllowMatch::Net(ip, if ip.is_ipv4() { 32 } else { 128 }));
    }
    let valid = !entry.is_empty()
        && entry.len() <= 253
        && entry.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    valid.then_some(AllowMatch::Host(entry))
}

fn in_net(ip: std::net::IpAddr, net: std::net::IpAddr, bits: u8) -> bool {
    use std::net::IpAddr;
    match (unmapped(ip), unmapped(net)) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - u32::from(bits)) };
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let mask = if bits == 0 { 0 } else { u128::MAX << (128 - u32::from(bits)) };
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

fn unmapped(ip: std::net::IpAddr) -> std::net::IpAddr {
    match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map(std::net::IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

/// Addresses NO target may reach, whatever the allowlist says: loopback,
/// link-local, the cloud metadata endpoints, unspecified, multicast and
/// broadcast. The node's own services and the instance credentials of its
/// cloud live there.
pub fn always_refused(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match unmapped(ip) {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_multicast()
                || v4.is_broadcast() || v4.octets()[0] == 0
                // Alibaba's and Oracle's metadata endpoints (AWS/GCP/Azure use
                // 169.254.169.254, link-local above).
                || v4.octets() == [100, 100, 100, 200]
                || v4.octets() == [192, 0, 0, 192]
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast()
                || (seg[0] & 0xffc0) == 0xfe80
                // AWS's IPv6 metadata endpoint.
                || v6 == "fd00:ec2::254".parse::<std::net::Ipv6Addr>().expect("literal")
        }
    }
}

/// Where a pass may send: public addresses, plus the platform admin's
/// allowlist (`ALLOWLIST_SETTING`), never an `always_refused` address.
#[derive(Debug, Clone, Default)]
pub struct AddressPolicy {
    allowlist: Vec<AllowEntry>,
    /// The tests' collectors listen on loopback, and speak plain HTTP.
    #[cfg(test)]
    loopback_for_tests: bool,
}

impl AddressPolicy {
    /// Public addresses only.
    pub fn public() -> Self {
        Self::default()
    }

    pub fn with_allowlist(allowlist: Vec<AllowEntry>) -> Self {
        Self { allowlist, ..Self::default() }
    }

    /// The platform admin's list as the main database holds it; an unreadable
    /// or malformed setting is an EMPTY list (public addresses only).
    pub fn load(main_db: &DbPool) -> Self {
        let list = crate::db::repository::get_setting(main_db, ALLOWLIST_SETTING)
            .ok()
            .flatten()
            .and_then(|json| parse_allowlist(&json).ok())
            .unwrap_or_default();
        Self::with_allowlist(list)
    }

    #[cfg(test)]
    pub fn loopback_for_tests() -> Self {
        Self { loopback_for_tests: true, ..Self::default() }
    }

    #[cfg(test)]
    fn test_loopback(&self, ip: std::net::IpAddr) -> bool {
        self.loopback_for_tests && unmapped(ip).is_loopback()
    }
    #[cfg(not(test))]
    fn test_loopback(&self, _ip: std::net::IpAddr) -> bool {
        false
    }

    fn host_entry(&self, host: &str, port: u16) -> Option<&AllowEntry> {
        let host = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
        self.allowlist.iter().find(|e| {
            e.port.is_none_or(|p| p == port)
                && matches!(allow_match(&e.entry), Some(AllowMatch::Host(h)) if h == host)
        })
    }

    fn net_entry(&self, ip: std::net::IpAddr, port: u16) -> Option<&AllowEntry> {
        self.allowlist.iter().find(|e| {
            e.port.is_none_or(|p| p == port)
                && matches!(allow_match(&e.entry), Some(AllowMatch::Net(net, bits)) if in_net(ip, net, bits))
        })
    }

    /// Whether `ip`, an address `host` resolved to, may be sent to.
    fn allows(&self, host: &str, ip: std::net::IpAddr, port: u16) -> bool {
        if self.test_loopback(ip) {
            return true;
        }
        if always_refused(ip) {
            return false;
        }
        address_allowed(ip) || self.host_entry(host, port).is_some() || self.net_entry(ip, port).is_some()
    }

    /// Whether plain `http://` may be used to `host` (reached at `addrs`):
    /// only to a listed entry the platform admin marked "allow http" — by
    /// its name, or every address it reaches inside such a network.
    fn allows_http(&self, host: &str, addrs: &[std::net::IpAddr], port: u16) -> bool {
        if addrs.iter().all(|ip| self.test_loopback(*ip)) && !addrs.is_empty() {
            return true;
        }
        if self.host_entry(host, port).is_some_and(|e| e.allow_http) {
            return true;
        }
        !addrs.is_empty() && addrs.iter().all(|ip| self.net_entry(*ip, port).is_some_and(|e| e.allow_http))
    }
}

/// Parses and checks the platform admin's list (`SettingsUpdateRequest`
/// refuses a malformed one): every entry is a host name, an address or a
/// network, and none of them an address no target may ever reach.
pub fn parse_allowlist(json: &str) -> Result<Vec<AllowEntry>> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let list: Vec<AllowEntry> = serde_json::from_str(json).map_err(|_| anyhow!("{FORWARD_ALLOWLIST_INVALID}"))?;
    for e in &list {
        if e.port == Some(0) {
            return Err(anyhow!("{FORWARD_ALLOWLIST_INVALID}"));
        }
        match allow_match(&e.entry) {
            None => return Err(anyhow!("{FORWARD_ALLOWLIST_INVALID}")),
            // A network wider than /16 (IPv4) or /48 (IPv6) is refused: one
            // typo must not open the operator's whole LAN (critic C1).
            Some(AllowMatch::Net(ip, bits)) if bits < if ip.is_ipv4() { 16 } else { 48 } => {
                return Err(anyhow!("{FORWARD_ALLOWLIST_TOO_WIDE}"))
            }
            Some(AllowMatch::Net(ip, _)) if always_refused(ip) => {
                return Err(anyhow!("{FORWARD_ALLOWLIST_REFUSED}"))
            }
            Some(AllowMatch::Host(h)) if h == "localhost" || h.ends_with(".localhost") => {
                return Err(anyhow!("{FORWARD_ALLOWLIST_REFUSED}"))
            }
            _ => {}
        }
    }
    Ok(list)
}

/// A list entry that is not a host name, an address or a network.
pub const FORWARD_ALLOWLIST_INVALID: &str = "refusal:forward_allowlist_invalid";
/// A network wider than /16 (IPv4) or /48 (IPv6).
pub const FORWARD_ALLOWLIST_TOO_WIDE: &str = "refusal:forward_allowlist_too_wide";
/// A list entry no target may ever reach (loopback, link-local, metadata).
pub const FORWARD_ALLOWLIST_REFUSED: &str = "refusal:forward_allowlist_refused";

/// Why a send failed, as the card may show it: a code the screen words.
/// `forward:http_status:<n>` for a public endpoint that answered with an
/// error, `forward:not_accepted` for everything else — whether the address
/// was refused, did not resolve, did not connect or timed out is NOT said.
#[derive(Debug)]
struct NotAccepted(String);

impl std::fmt::Display for NotAccepted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotAccepted {}

const NOT_ACCEPTED: &str = "forward:not_accepted";

fn not_accepted() -> anyhow::Error {
    anyhow::Error::new(NotAccepted(NOT_ACCEPTED.to_string()))
}

/// A stored error as the card may show it: one of the two neutral codes. An
/// older node stored its raw error text; that text is not shown.
fn neutral_error(stored: &str) -> String {
    if stored.is_empty() {
        String::new()
    } else if stored == NOT_ACCEPTED || stored.starts_with("forward:http_status:") {
        stored.to_string()
    } else {
        NOT_ACCEPTED.to_string()
    }
}

/// Resolves `host:port` and returns the one address the send may use — the
/// first, and only when EVERY address the name resolves to is allowed: a name
/// that also resolves to a refused address is refused outright rather than
/// raced against it. `http` asks for plain HTTP, which only an allowlist
/// entry marked "allow http" permits.
async fn checked_address(host: &str, port: u16, policy: &AddressPolicy, http: bool) -> Result<std::net::SocketAddr> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<std::net::SocketAddr> = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![std::net::SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| {
                tracing::info!("tentanas forwarding: '{host}' does not resolve: {e}");
                not_accepted()
            })?
            .collect(),
    };
    if addrs.is_empty() || !addrs.iter().all(|a| policy.allows(host, a.ip(), port)) {
        tracing::info!("tentanas forwarding: '{host}' resolves to an address a target may not use");
        return Err(not_accepted());
    }
    if http && !policy.allows_http(host, &addrs.iter().map(|a| a.ip()).collect::<Vec<_>>(), port) {
        tracing::info!("tentanas forwarding: plain http to '{host}' is not allowed");
        return Err(not_accepted());
    }
    Ok(addrs[0])
}

/// `host:port`, with a port that fits. A literal address must be one a
/// target may use (public, or on the platform admin's list); a host NAME is
/// resolved when it is used, not here — a name that resolves later is a
/// valid target, and a DNS lookup is not a validation.
pub fn validate_syslog_target(target: &str, policy: &AddressPolicy) -> Result<()> {
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
    let port = match port.parse::<u16>() {
        Ok(p) if p > 0 => p,
        _ => return Err(anyhow!("'{port}' is not a port")),
    };
    if let Ok(ip) = host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        if !policy.allows(host, ip, port) {
            return Err(anyhow!("{FORWARD_ADDRESS_REFUSED}"));
        }
    }
    Ok(())
}

/// The refusal of a literal address a target may not use (loopback, a
/// private network not on the platform admin's list, …), worded by the
/// screen. Only what the admin typed is judged here; a host name is judged
/// when it is used.
pub const FORWARD_ADDRESS_REFUSED: &str = "refusal:forward_address_refused";
/// The refusal of a webhook that is not `https://` (plain HTTP only to a
/// listed internal host marked "allow http").
pub const FORWARD_HTTPS_ONLY: &str = "refusal:forward_https_only";

/// An `https://` endpoint — or `http://` to an entry of the platform admin's
/// list marked "allow http". The payload names the tenant's disks, arrays and
/// files, and a webhook URL is a secret that plain HTTP would hand to every
/// hop on the way.
pub fn validate_webhook_url(url: &str, policy: &AddressPolicy) -> Result<()> {
    let http = url.starts_with("http://");
    if !url.starts_with("https://") && !http {
        return Err(anyhow!("{FORWARD_HTTPS_ONLY}"));
    }
    if url.len() > 2048 || url.contains(['\n', '\r', ' ']) {
        return Err(anyhow!("the webhook URL is not a single plain URL"));
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| anyhow!("the webhook URL is not a URL"))?;
    let Some(host) = parsed.host() else {
        return Err(anyhow!("the webhook URL has no host"));
    };
    let literal = match host {
        url::Host::Ipv4(ip) => Some(std::net::IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(std::net::IpAddr::V6(ip)),
        url::Host::Domain(_) => None,
    };
    let host_str = parsed.host_str().unwrap_or_default();
    let port = parsed.port_or_known_default().unwrap_or(0);
    if let Some(ip) = literal {
        if !policy.allows(host_str, ip, port) {
            return Err(anyhow!("{FORWARD_ADDRESS_REFUSED}"));
        }
    }
    // Plain http: only to a listed entry marked "allow http" — by name, or a
    // literal inside such a network. A name only on a network entry is
    // judged when it is used (`checked_address`).
    if http {
        let by_name = policy.host_entry(host_str, port).is_some_and(|e| e.allow_http);
        let by_net = literal.is_some_and(|ip| policy.net_entry(ip, port).is_some_and(|e| e.allow_http));
        if !by_name && !by_net && !literal.is_some_and(|ip| policy.test_loopback(ip)) {
            return Err(anyhow!("{FORWARD_HTTPS_ONLY}"));
        }
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

/// Forwards one batch to every target that is on. Called once a minute from
/// the schedule loop. One target's failure is its own: it is recorded on its
/// cursor, as a neutral code, and the others are still served.
pub async fn forward_tick(main_db: &DbPool, nas_db: &DbPool) {
    let Some(addon_id) = crate::db::repository::get_package_instance(main_db, super::PACKAGE_ID)
        .ok()
        .flatten()
        .map(|(addon_id, _)| addon_id)
    else {
        return;
    };
    let (node, orgs) = stored_all(main_db, &addon_id);
    // A soft-deleted organisation's target is not served: its admins are gone,
    // and the node-wide alerts it would receive are nobody's business there.
    let deleted: std::collections::BTreeSet<String> = crate::services::org::list_organizations(main_db, None)
        .map(|all| all.into_iter().filter(|o| o.status == "deleted").map(|o| o.org_id).collect())
        .unwrap_or_default();
    let mut targets: Vec<(Target<'_>, &Stored)> = Vec::new();
    if let Some(node) = node.as_ref() {
        targets.push((Target::Node, node));
    }
    for (org, s) in &orgs {
        if !deleted.contains(org) {
            targets.push((Target::Org(org), s));
        }
    }
    // The platform admin's list of internal collectors, read once per pass.
    let policy = AddressPolicy::load(main_db);
    for (target, s) in targets {
        if !s.sends() {
            continue;
        }
        if let Err(e) = forward_once(nas_db, target, s, &policy).await {
            // The node's log keeps the whole story; the card gets a code.
            tracing::warn!("tentanas: alert forwarding failed: {e:#}");
            let code = e.downcast_ref::<NotAccepted>().map(|n| n.0.clone()).unwrap_or_else(|| NOT_ACCEPTED.to_string());
            let _ = store::record_forward_error(nas_db, target.cursor_key(), &code);
        }
    }
}

/// One batch for one target. Returns how many rows left the node.
async fn forward_once(nas_db: &DbPool, target: Target<'_>, s: &Stored, policy: &AddressPolicy) -> Result<usize> {
    let key = target.cursor_key();
    let include_access = s.include_access && target != Target::Node;
    let mut cursor = match store::forward_cursor(nas_db, key)? {
        Some(c) if c.enabled_at == s.enabled_at => c,
        _ => store::place_forward_cursor(nas_db, key, &s.enabled_at)?,
    };
    // Access lines asked for later start from THAT moment, not from the
    // switch-on of the target (critic wave 9b, MINOR 1).
    if include_access && cursor.access_enabled_at != s.access_enabled_at {
        cursor = store::place_forward_access(nas_db, key, &s.access_enabled_at)?;
    }
    // The scope is part of the batch query itself (`store::forward_batch`):
    // another organisation's row cannot be selected for this target.
    let rows = store::forward_batch(nas_db, key, include_access, &cursor, BATCH)?;
    if rows.is_empty() {
        return Ok(0);
    }
    let hostname = hostname();
    let node_id = crate::sync::runtime::local_node_id().unwrap_or_else(|| "local".to_string());
    if !s.syslog_target.is_empty() {
        send_syslog(&s.syslog_target, &rows, &hostname, &node_id, policy).await?;
    }
    if !s.webhook_url.is_empty() {
        send_webhook(&s.webhook_url, &rows, &hostname, &node_id, policy).await?;
    }
    // Moved only now: both transports agreed the batch left the node.
    store::advance_forward_cursor(nas_db, key, &rows)?;
    Ok(rows.len())
}

async fn send_syslog(
    target: &str,
    rows: &[ForwardRow],
    hostname: &str,
    node_id: &str,
    policy: &AddressPolicy,
) -> Result<()> {
    let (host, port) = target.rsplit_once(':').ok_or_else(not_accepted)?;
    let port: u16 = port.parse().map_err(|_| not_accepted())?;
    let addr = checked_address(host, port, policy, false).await?;
    let bind = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let socket = tokio::net::UdpSocket::bind(bind).await.map_err(|e| {
        tracing::warn!("tentanas forwarding: no local UDP socket: {e}");
        not_accepted()
    })?;
    // Connected to the address that was checked, never to the name again.
    socket.connect(addr).await.map_err(|e| {
        tracing::info!("tentanas forwarding: syslog target not reachable: {e}");
        not_accepted()
    })?;
    for row in rows {
        let line = syslog_line(row, hostname, node_id);
        socket.send(line.as_bytes()).await.map_err(|e| {
            tracing::info!("tentanas forwarding: syslog send failed: {e}");
            not_accepted()
        })?;
    }
    Ok(())
}

async fn send_webhook(
    url: &str,
    rows: &[ForwardRow],
    hostname: &str,
    node_id: &str,
    policy: &AddressPolicy,
) -> Result<()> {
    // An address stored before the https-only rule (or before its entry left
    // the list) is not sent to.
    validate_webhook_url(url, policy).map_err(|_| not_accepted())?;
    let parsed = reqwest::Url::parse(url).map_err(|_| not_accepted())?;
    let host = parsed.host_str().ok_or_else(not_accepted)?.to_string();
    let port = parsed.port_or_known_default().ok_or_else(not_accepted)?;
    let addr = checked_address(&host, port, policy, parsed.scheme() == "http").await?;
    let body = webhook_body(rows, hostname, node_id);
    let client = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        // Pinned: the connection goes to the address that was checked, even
        // if the name resolves elsewhere a moment later (DNS rebinding).
        .resolve(&host.trim_start_matches('[').trim_end_matches(']').to_string(), addr)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|e| {
            tracing::warn!("tentanas forwarding: HTTP client: {e}");
            not_accepted()
        })?;
    let response = client.post(parsed).json(&body).send().await.map_err(|e| {
        tracing::info!("tentanas forwarding: webhook POST failed: {e}");
        not_accepted()
    })?;
    if !response.status().is_success() {
        // A redirect is an answer that was not an acceptance, like any other.
        return Err(anyhow::Error::new(NotAccepted(format!(
            "forward:http_status:{}",
            response.status().as_u16()
        ))));
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

    /// Two organisations with one array each, and one share each.
    fn two_tenants(p: &DbPool) {
        let conn = p.write().expect("write");
        conn.execute_batch(
            "INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at) VALUES
               ('arr-a','org-a','nas','alpha','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z'),
               ('arr-b','org-b','nas','bravo','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z');
             INSERT INTO nas_shares (share_id,name,protocol,source_path,created_at,updated_at,org_id) VALUES
               ('s1','projekty','smb','/mnt/tank/projekty','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z','org-a'),
               ('s2','kadry','smb','/mnt/tank/kadry','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z','org-b');",
        )
        .expect("tenants");
    }

    fn access(share: &str, target: &str) -> NasAccessEvent {
        NasAccessEvent {
            at: "2026-09-03T12:12:04Z".to_string(),
            share: share.to_string(),
            user: "anna".to_string(),
            client: "10.10.0.24".to_string(),
            operation: "unlinkat".to_string(),
            result: "fail".to_string(),
            target: target.to_string(),
            detail: "NT_STATUS_ACCESS_DENIED".to_string(),
            event_id: 0,
        }
    }

    /// A target that was switched on long before any of the rows below, so
    /// every row is raised while it is on.
    fn on(collector: &tokio::net::UdpSocket, include_access: bool) -> Stored {
        Stored {
            enabled: true,
            syslog_target: collector.local_addr().expect("addr").to_string(),
            include_access,
            enabled_at: "2000-01-01T00:00:00Z".to_string(),
            ..Default::default()
        }
    }

    /// Every datagram the collector receives until it stays quiet.
    async fn received(collector: &tokio::net::UdpSocket) -> Vec<String> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 4096];
        while let Ok(Ok(n)) =
            tokio::time::timeout(Duration::from_millis(300), collector.recv(&mut buf)).await
        {
            out.push(String::from_utf8_lossy(&buf[..n]).into_owned());
        }
        out
    }

    /// Each organisation's target receives exactly what that organisation may
    /// read: its own alert and the node-wide one, never the other tenant's.
    /// The node-wide target keeps receiving the node-wide alert only.
    #[tokio::test]
    async fn each_organisation_receives_its_own_alerts_and_the_node_wide_ones_only() {
        let p = db();
        two_tenants(&p);
        store::raise_alert(&p, "elastic:alpha:sync", "warning", "elastic-array", "alpha", "Macierz alpha: sync failed", "")
            .expect("alert");
        store::raise_alert(&p, "elastic:bravo:sync", "critical", "elastic-array", "bravo", "Macierz bravo: disk missing", "")
            .expect("alert");
        store::raise_alert(&p, "disk:a:health", "warning", "disk", "a", "Disk sda: warning", "2 reallocated sectors")
            .expect("alert");

        let a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("socket a");
        let b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("socket b");
        let node = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("socket node");
        assert_eq!(forward_once(&p, Target::Org("org-a"), &on(&a, false), &AddressPolicy::loopback_for_tests()).await.expect("a"), 2);
        assert_eq!(forward_once(&p, Target::Org("org-b"), &on(&b, false), &AddressPolicy::loopback_for_tests()).await.expect("b"), 2);
        assert_eq!(forward_once(&p, Target::Node, &on(&node, false), &AddressPolicy::loopback_for_tests()).await.expect("node"), 1);

        let a_lines = received(&a).await;
        assert_eq!(a_lines.len(), 2, "{a_lines:#?}");
        assert!(a_lines.iter().any(|l| l.contains("Disk sda: warning")), "{a_lines:#?}");
        assert!(a_lines.iter().any(|l| l.contains("alpha")), "{a_lines:#?}");
        assert!(!a_lines.iter().any(|l| l.contains("bravo")), "org-b's alert reached org-a: {a_lines:#?}");
        let b_lines = received(&b).await;
        assert!(b_lines.iter().any(|l| l.contains("bravo")), "{b_lines:#?}");
        assert!(!b_lines.iter().any(|l| l.contains("alpha")), "org-a's alert reached org-b: {b_lines:#?}");
        let node_lines = received(&node).await;
        assert_eq!(node_lines.len(), 1, "{node_lines:#?}");
        assert!(node_lines[0].contains("Disk sda: warning"), "{node_lines:#?}");

        // Nothing is sent twice: every cursor moved past what it sent.
        for (target, socket) in [(Target::Org("org-a"), &a), (Target::Org("org-b"), &b), (Target::Node, &node)] {
            assert_eq!(forward_once(&p, target, &on(socket, false), &AddressPolicy::loopback_for_tests()).await.expect("again"), 0);
        }
    }

    /// An organisation that asked for its access lines gets the lines of its
    /// OWN shares — never another tenant's — and the node-wide target never
    /// gets an access line, whatever its stored switch says.
    #[tokio::test]
    async fn access_lines_reach_only_the_owning_organisation() {
        let p = db();
        two_tenants(&p);
        store::insert_access_events(&p, &[access("projekty", "raport.xlsx"), access("kadry", "place.xlsx")])
            .expect("events");

        let a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("socket a");
        let quiet = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("socket quiet");
        let node = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("socket node");
        let cursor = store::place_forward_cursor(&p, "org-a", "2000-01-01T00:00:00Z").expect("cursor");
        assert_eq!(store::forward_pending(&p, "org-a", true, &cursor).expect("pending"), 1);
        assert_eq!(store::forward_pending(&p, "org-a", false, &cursor).expect("pending"), 0);

        assert_eq!(forward_once(&p, Target::Org("org-a"), &on(&a, true), &AddressPolicy::loopback_for_tests()).await.expect("a"), 1);
        let lines = received(&a).await;
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert!(lines[0].contains("raport.xlsx on projekty by anna"), "{lines:#?}");
        assert!(lines[0].contains("kind=\"access\""), "{lines:#?}");

        // Without the switch the same organisation gets no access line.
        assert_eq!(forward_once(&p, Target::Org("org-c"), &on(&quiet, false), &AddressPolicy::loopback_for_tests()).await.expect("c"), 0);
        // The node-wide target: no access line even with the flag stored on.
        assert_eq!(forward_once(&p, Target::Node, &on(&node, true), &AddressPolicy::loopback_for_tests()).await.expect("node"), 0);
        assert!(received(&node).await.is_empty(), "an access line reached the node-wide target");
    }

    /// A target sends what is raised while it is on: switched on for the
    /// first time it does not receive the node's history, and switched on
    /// again it does not receive what happened while it was off.
    #[tokio::test]
    async fn a_target_sends_only_what_was_raised_while_it_was_on() {
        let p = db();
        store::raise_alert(&p, "disk:a:health", "warning", "disk", "a", "Disk sda: warning", "").expect("alert");
        {
            let conn = p.write().expect("write");
            conn.execute("UPDATE nas_alerts SET raised_at = '2026-09-01T00:00:00Z'", []).expect("backdate");
        }
        let collector = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("socket");
        let mut s = on(&collector, false);
        s.enabled_at = "2026-09-02T00:00:00Z".to_string();
        assert_eq!(forward_once(&p, Target::Org("org-a"), &s, &AddressPolicy::loopback_for_tests()).await.expect("pass"), 0, "history is not sent");

        store::raise_alert(&p, "disk:b:health", "warning", "disk", "b", "Disk sdb: warning", "").expect("alert");
        assert_eq!(forward_once(&p, Target::Org("org-a"), &s, &AddressPolicy::loopback_for_tests()).await.expect("pass"), 1);
        assert!(received(&collector).await[0].contains("Disk sdb"));

        // Off, an alert, on again: the cursor is placed for the new switch-on.
        store::raise_alert(&p, "disk:c:health", "warning", "disk", "c", "Disk sdc: warning", "").expect("alert");
        {
            let conn = p.write().expect("write");
            conn.execute("UPDATE nas_alerts SET raised_at = '2026-09-03T00:00:00Z' WHERE subject_id = 'c'", [])
                .expect("date");
        }
        s.enabled_at = "2026-09-04T00:00:00Z".to_string();
        assert_eq!(forward_once(&p, Target::Org("org-a"), &s, &AddressPolicy::loopback_for_tests()).await.expect("pass"), 0, "the pause is not replayed");
    }

    /// A send that fails moves no cursor: the rows go out on the next pass
    /// (at least once), and the error is the target's own.
    #[tokio::test]
    async fn a_failed_send_keeps_the_rows_for_the_next_pass() {
        let p = db();
        store::raise_alert(&p, "disk:a:health", "warning", "disk", "a", "Disk sda: warning", "").expect("alert");
        let s = Stored {
            enabled: true,
            webhook_url: "http://127.0.0.1:9/unreachable".to_string(),
            enabled_at: "2000-01-01T00:00:00Z".to_string(),
            ..Default::default()
        };
        let err = forward_once(&p, Target::Org("org-a"), &s, &AddressPolicy::loopback_for_tests()).await.expect_err("refused");
        assert_eq!(err.to_string(), NOT_ACCEPTED, "the card gets a neutral code, never the socket error");
        let cursor = store::forward_cursor(&p, "org-a").expect("read").expect("cursor");
        assert_eq!(store::forward_pending(&p, "org-a", false, &cursor).expect("pending"), 1);
        assert!(cursor.last_sent_at.is_none());
    }

    #[test]
    fn a_target_is_checked_when_it_is_saved_not_when_it_is_used() {
        // A host name is judged when it is used; a literal must be public.
        for good in ["siem.local:514", "203.0.114.9:1514", "[2001:db9::1]:514"] {
            assert!(validate_syslog_target(good, &AddressPolicy::public()).is_ok(), "{good}");
        }
        for bad in ["siem.local", "siem.local:0", "siem.local:70000", ":514", "a b:514"] {
            assert!(validate_syslog_target(bad, &AddressPolicy::public()).is_err(), "{bad}");
        }
        assert!(validate_webhook_url("https://siem.local/hook", &AddressPolicy::public()).is_ok());
        for bad in ["ftp://x/y", "siem.local/hook", "https://x/y z"] {
            assert!(validate_webhook_url(bad, &AddressPolicy::public()).is_err(), "{bad}");
        }
    }

    fn access_row() -> ForwardRow {
        ForwardRow {
            kind: "access",
            id: "42".to_string(),
            seq: 42,
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

    /// The syslog transport against a REAL socket: the collector is a UDP
    /// socket on loopback, and the frames arrive as they were built.
    #[tokio::test]
    async fn the_batch_reaches_a_real_udp_collector() {
        let collector = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("collector socket");
        let target = collector.local_addr().expect("addr").to_string();
        assert!(validate_syslog_target(&target, &AddressPolicy::public()).is_err(), "a loopback literal is refused when saved");

        let rows = vec![access_row(), access_row()];
        send_syslog(&target, &rows, "helios", "node-1", &AddressPolicy::loopback_for_tests())
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

    // ----- wave 9b round 2: where a target may point (MAJOR 4) -------------------

    #[test]
    fn a_target_may_not_point_into_the_operators_networks() {
        use std::net::IpAddr;
        for bad in ["127.0.0.1", "10.1.2.3", "172.16.0.9", "192.168.1.1", "169.254.169.254", "100.64.0.1",
            "0.0.0.0", "224.0.0.1", "255.255.255.255", "::1", "::", "fe80::1", "fd00::1", "fc00::1", "ff02::1",
            "::ffff:10.0.0.5", "::ffff:127.0.0.1"] {
            assert!(!address_allowed(bad.parse::<IpAddr>().unwrap()), "{bad} must be refused");
        }
        for good in ["1.1.1.1", "203.0.114.9", "2001:4860:4860::8888"] {
            assert!(address_allowed(good.parse::<IpAddr>().unwrap()), "{good} is public");
        }
        // Checked when saved, for what the admin typed literally.
        for bad in ["http://siem.example.com/hook", "https://127.0.0.1/hook", "https://[fd00::1]/x", "https://169.254.169.254/latest"] {
            assert!(validate_webhook_url(bad, &AddressPolicy::public()).is_err(), "{bad}");
        }
        assert!(validate_webhook_url("https://siem.example.com/hook", &AddressPolicy::public()).is_ok());
        for bad in ["127.0.0.1:514", "10.0.0.5:514", "[::1]:514"] {
            assert!(validate_syslog_target(bad, &AddressPolicy::public()).is_err(), "{bad}");
        }
        assert!(validate_syslog_target("siem.example.com:514", &AddressPolicy::public()).is_ok(), "a name is judged when it is used");
    }

    /// The production policy refuses a loopback collector at SEND time — a
    /// name that resolves there, or a literal stored before the rule — and
    /// the card is told only "not accepted".
    #[tokio::test]
    async fn the_public_policy_sends_nothing_to_a_loopback_collector() {
        let p = db();
        store::raise_alert(&p, "disk:a:health", "warning", "disk", "a", "Disk sda: warning", "").expect("alert");
        let collector = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("socket");
        for s in [on(&collector, false), Stored {
            syslog_target: format!("localhost:{}", collector.local_addr().unwrap().port()),
            ..on(&collector, false)
        }] {
            let err = forward_once(&p, Target::Org("org-a"), &s, &AddressPolicy::public()).await.expect_err("refused");
            assert_eq!(err.to_string(), NOT_ACCEPTED);
        }
        assert!(received(&collector).await.is_empty(), "nothing reached the loopback collector");
    }

    /// A tiny HTTP endpoint: answers every request with `status` and
    /// `location`, and counts the connections it got.
    async fn http_endpoint(status: &'static str, location: String) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = vec![0u8; 16384];
                let _ = sock.read(&mut buf).await;
                let reply = format!("HTTP/1.1 {status}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        (port, hits)
    }

    /// Redirects are not followed — a 302 to the node's own port is just an
    /// answer that was not an acceptance — and what the card keeps is the
    /// status alone.
    #[tokio::test]
    async fn a_redirect_is_not_followed_and_only_the_status_is_kept() {
        let p = db();
        store::raise_alert(&p, "disk:a:health", "warning", "disk", "a", "Disk sda: warning", "").expect("alert");
        let (inner, inner_hits) = http_endpoint("200 OK", String::new()).await;
        let (outer, outer_hits) = http_endpoint("302 Found", format!("http://127.0.0.1:{inner}/")).await;
        let s = Stored {
            enabled: true,
            webhook_url: format!("http://127.0.0.1:{outer}/hook"),
            enabled_at: "2000-01-01T00:00:00Z".to_string(),
            ..Default::default()
        };
        let err = forward_once(&p, Target::Org("org-a"), &s, &AddressPolicy::loopback_for_tests()).await.expect_err("not accepted");
        assert_eq!(err.to_string(), "forward:http_status:302");
        assert_eq!(outer_hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(inner_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "the redirect was not followed");
        // A raw error an older node stored is shown as the neutral code.
        assert_eq!(neutral_error("connection refused to 10.0.0.5:22"), NOT_ACCEPTED);
        assert_eq!(neutral_error("forward:http_status:404"), "forward:http_status:404");
    }

    /// A webhook URL is a secret: an admin sees scheme and host, a reader
    /// neither address; a save that sends the mask back keeps the stored URL.
    #[test]
    fn a_webhook_url_never_goes_back_whole() {
        assert_eq!(mask_webhook("https://hooks.slack.com/services/T0/B1/SECRET"), "https://hooks.slack.com/…");
        assert_eq!(mask_webhook("https://user:pw@siem.example.com:8443/in?token=x"), "https://siem.example.com:8443/…");
        assert_eq!(mask_webhook(""), "");
    }

    /// Access lines asked for LATER start from that moment: the log retained
    /// from before is not sent (critic wave 9b, MINOR 1).
    #[tokio::test]
    async fn access_lines_turned_on_later_do_not_replay_the_retained_log() {
        let p = db();
        two_tenants(&p);
        let mut old = access("projekty", "stary.xlsx");
        old.at = "2026-09-02T00:00:00Z".to_string();
        let mut new = access("projekty", "nowy.xlsx");
        new.at = "2026-09-04T00:00:00Z".to_string();
        store::insert_access_events(&p, &[old, new]).expect("events");
        let collector = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("socket");
        let mut s = on(&collector, false);
        s.enabled_at = "2026-09-01T00:00:00Z".to_string();
        assert_eq!(forward_once(&p, Target::Org("org-a"), &s, &AddressPolicy::loopback_for_tests()).await.expect("pass"), 0);
        s.include_access = true;
        s.access_enabled_at = "2026-09-03T00:00:00Z".to_string();
        assert_eq!(forward_once(&p, Target::Org("org-a"), &s, &AddressPolicy::loopback_for_tests()).await.expect("pass"), 1);
        let lines = received(&collector).await;
        assert!(lines[0].contains("nowy.xlsx"), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("stary.xlsx")), "the older line is not replayed");
    }

    /// MAJOR 5: a webhook URL is a secret. Saved by an organisation's admin,
    /// read back masked by that admin, not at all by a reader; a save that
    /// sends the mask back keeps the stored URL; the retired node-wide target
    /// can only be deleted.
    #[test]
    fn a_webhook_url_is_stored_whole_and_read_back_masked_or_not_at_all() {
        let main = crate::dispatch::state::AppState::for_test().db.clone();
        let nas = db();
        let addon = "tentanas-1a2b3c4d";
        let url = "https://hooks.slack.com/services/T0/B1/SECRET";
        let saved = set_settings(&main, &nas, addon, "u1", "org-a", true, "", url, false).expect("save");
        assert_eq!(saved.webhook_url, "https://hooks.slack.com/…", "the answer to the save is masked too");
        assert_eq!(stored(&main, addon, Target::Org("org-a")).unwrap().webhook_url, url, "stored whole");
        let reader = settings(&main, &nas, addon, Target::Org("org-a"), Viewer::Reader);
        assert!(reader.enabled);
        assert_eq!((reader.webhook_url.as_str(), reader.syslog_target.as_str()), ("", ""), "a reader gets no address");
        // The dialog sends back what it was shown: the stored URL stays.
        set_settings(&main, &nas, addon, "u1", "org-a", true, "", "https://hooks.slack.com/…", true).expect("resave");
        assert_eq!(stored(&main, addon, Target::Org("org-a")).unwrap().webhook_url, url);
        assert!(set_settings(&main, &nas, addon, "u1", "org-a", true, "", "http://siem.example.com/x", false).is_err(), "https only");

        crate::db::repository::upsert_addon_config_value(&main, addon, SETTINGS_KEY,
            r#"{"enabled":true,"syslog_target":"legacy.example.com:514","webhook_url":"https://legacy.example.com/in/SECRET","include_access":false}"#,
            false, None).expect("legacy");
        let node = settings(&main, &nas, addon, Target::Node, Viewer::Admin);
        assert_eq!(node.webhook_url, "https://legacy.example.com/…");
        delete_node_target(&main, &nas, addon).expect("delete");
        assert!(stored(&main, addon, Target::Node).is_none(), "deleted");
    }

    /// Owner decision (round 3): the platform admin's list of internal
    /// collectors. Listed private hosts, addresses and networks are allowed;
    /// loopback, link-local and metadata never are, listed or not; plain
    /// http only to an entry marked "allow http".
    #[test]
    fn the_platform_allowlist_opens_listed_internal_collectors_only() {
        use std::net::IpAddr;
        let list = parse_allowlist(r#"[{"entry":"siem.lan","allow_http":true},{"entry":"logs.lan"},{"entry":"10.0.5.0/24"},{"entry":"fd00:ec2::/48"},{"entry":"10.9.9.9","allow_http":true}]"#)
            .expect("a valid list");
        let policy = AddressPolicy::with_allowlist(list);
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(policy.allows("siem.lan", ip("192.168.7.7"), 443), "a listed name may resolve privately");
        assert!(policy.allows("x.example", ip("10.0.5.20"), 443), "an address in a listed network");
        assert!(!policy.allows("x.example", ip("10.0.6.20"), 443), "one outside it is not");
        assert!(!policy.allows("x.example", ip("fd00:ec2::254"), 443), "the metadata endpoint, even inside a listed network");
        assert!(!policy.allows("siem.lan", ip("127.0.0.1"), 443), "loopback, even behind a listed name");
        assert!(!policy.allows("siem.lan", ip("169.254.169.254"), 443), "link-local metadata, even behind a listed name");
        // Saved targets.
        assert!(validate_syslog_target("10.0.5.20:514", &policy).is_ok());
        assert!(validate_syslog_target("10.0.6.20:514", &policy).is_err());
        assert!(validate_webhook_url("http://siem.lan/hook", &policy).is_ok(), "http to an entry that allows it");
        assert!(validate_webhook_url("http://10.9.9.9/hook", &policy).is_ok());
        assert!(validate_webhook_url("http://logs.lan/hook", &policy).is_err(), "a listed name without allow-http takes https only");
        assert!(validate_webhook_url("https://logs.lan/hook", &policy).is_ok());
        assert!(validate_webhook_url("http://10.0.5.20/hook", &policy).is_err(), "https only to an entry without allow-http");
        assert!(validate_webhook_url("https://10.0.5.20/hook", &policy).is_ok());
        assert!(validate_webhook_url("http://public.example.com/hook", &policy).is_err());
        // The list itself refuses what no target may reach.
        for bad in [r#"[{"entry":"127.0.0.1"}]"#, r#"[{"entry":"169.254.169.254"}]"#, r#"[{"entry":"fd00:ec2::254"}]"#,
            r#"[{"entry":"localhost"}]"#, r#"[{"entry":"not a host"}]"#, r#"[{"entry":"10.0.0.0/99"}]"#, "{}"] {
            assert!(parse_allowlist(bad).is_err(), "{bad}");
        }
        assert!(parse_allowlist("").unwrap().is_empty());
        // Round 4: no network wider than /16 or /48, and a port pins an entry.
        for wide in [r#"[{"entry":"10.0.0.0/8"}]"#, r#"[{"entry":"0.0.0.0/0"}]"#, r#"[{"entry":"fd00::/8"}]"#, r#"[{"entry":"10.0.0.0/15"}]"#] {
            assert!(parse_allowlist(wide).unwrap_err().to_string().contains("too_wide"), "{wide}");
        }
        assert!(parse_allowlist(r#"[{"entry":"10.0.5.0/16"},{"entry":"fd12:3456:789a::/48"}]"#).is_ok());
        let pinned = AddressPolicy::with_allowlist(parse_allowlist(r#"[{"entry":"siem.lan","port":6514}]"#).unwrap());
        assert!(validate_syslog_target("10.0.5.9:514", &pinned).is_err());
        assert!(pinned.allows("siem.lan", ip("10.0.5.9"), 6514), "the entry's own port");
        assert!(!pinned.allows("siem.lan", ip("10.0.5.9"), 22), "not another port of the same host");
        assert!(validate_webhook_url("https://siem.lan:6514/in", &pinned).is_ok());
        assert!(validate_webhook_url("https://siem.lan/in", &pinned).is_ok(), "a name is judged at send time");
        // The ranges round 2 missed (critic R2).
        for bad in ["198.18.0.1", "198.19.255.1", "192.88.99.1", "192.0.2.1", "198.51.100.1", "203.0.113.1", "2001:db8::1"] {
            assert!(!address_allowed(ip(bad)), "{bad}");
        }
    }

    /// The list is read from the platform setting on every pass, and a
    /// listed collector on loopback-free private space receives the batch
    /// only because it is listed (the send path uses the same policy).
    #[tokio::test]
    async fn a_pass_reads_the_platform_list() {
        let main = crate::dispatch::state::AppState::for_test().db.clone();
        crate::db::repository::set_setting(&main, ALLOWLIST_SETTING, r#"[{"entry":"10.0.5.0/24"}]"#).expect("setting");
        let policy = AddressPolicy::load(&main);
        assert!(policy.allows("x", "10.0.5.1".parse().unwrap(), 514));
        crate::db::repository::set_setting(&main, ALLOWLIST_SETTING, "not json").expect("setting");
        assert!(!AddressPolicy::load(&main).allows("x", "10.0.5.1".parse().unwrap(), 514), "a broken setting is an empty list");
    }

    /// Critic R3: a webhook stored before the https rule does not block
    /// saving the rest — switching the target off, changing its syslog — and
    /// is shown as needing a change.
    #[test]
    fn an_old_http_webhook_does_not_block_saving_the_rest() {
        let main = crate::dispatch::state::AppState::for_test().db.clone();
        let nas = db();
        let addon = "tentanas-1a2b3c4d";
        crate::db::repository::upsert_addon_config_value(&main, addon, &format!("{ORG_KEY_PREFIX}org-a"),
            r#"{"enabled":true,"syslog_target":"","webhook_url":"http://old.example.com/in/SECRET","include_access":false}"#,
            false, None).expect("old target");
        let shown = settings(&main, &nas, addon, Target::Org("org-a"), Viewer::Admin);
        assert!(shown.webhook_needs_migration);
        assert_eq!(shown.webhook_url, "http://old.example.com/…");
        let saved = set_settings(&main, &nas, addon, "u1", "org-a", false, "siem.example.com:514", "http://old.example.com/…", false)
            .expect("the rest saves");
        assert!(!saved.enabled);
        assert_eq!(saved.syslog_target, "siem.example.com:514");
        assert_eq!(stored(&main, addon, Target::Org("org-a")).unwrap().webhook_url, "http://old.example.com/in/SECRET", "kept, not used");
    }
}

