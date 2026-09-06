// ===== File: code_studio/egress/mod.rs — the only route a workspace has to the network =====
//
// `network_mode=bridge` filters nothing: it hands the sandbox a NAT and calls
// it isolation. The mechanism here is the opposite shape — the sandbox gets no
// default route, its single exit is this explicit HTTP/HTTPS proxy, and the
// proxy decides per request. It resolves DNS ITSELF and pins the answer for the
// whole operation, so the name the policy approved and the address the socket
// connects to cannot drift apart between the check and the connect.
//
// Three things are deliberately NOT here:
//
//   * No advisory allowlist. When the node has no enforcement mechanism,
//     `EgressGateway::for_workspace` returns `WorkspaceEgress::NoMechanism` and
//     the caller has to deal with that. A gateway object that "would have
//     filtered, if only traffic went through it" is audit fiction (§7.6).
//   * No protocol but HTTP and HTTPS. `ssh` and `git://` have no route out at
//     all; every git operation runs in the broker, outside the sandbox (§11).
//   * No database writes. Every screening result — allow and deny alike —
//     carries an `EgressEvent`, and the caller persists it into the session
//     event log. Writing here would duplicate that log.
//
// The blocked-destination set is shared with `remote_policy` rather than
// re-derived: cloud metadata, cluster control plane and loopback are blocked by
// what they ARE, not by who is asking (§11.4). Private and LAN addresses stay
// reachable through the allowlist, which is why the `web_research` guard is not
// reused for this — that one refuses every private address by design, and a
// workspace whose CI lives on the LAN would be unable to reach it.

pub mod control_socket;
pub mod firewall;
pub mod proxy;
pub mod resolver;
pub mod sni;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use url::Url;

use super::events::EventPayload;
use super::models::{AutonomyMode, EgressEnforcement, ExecMode};
use super::remote_policy::{is_forbidden_host_literal, is_forbidden_ip};
use resolver::Resolver;

/// How far the workspace may reach (§17.3). Stored as a string in the registry,
/// parsed here so an unknown value can never become "no restriction".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressPolicy {
    /// Local services of the owner node only. Enforceable exclusively where the
    /// sandbox has no route of its own.
    LocalOnly,
    /// Local services plus the providers on the organization list.
    OrgApproved,
    /// Local services plus whatever the workspace allowlist names.
    Any,
}

impl EgressPolicy {
    pub fn slug(self) -> &'static str {
        match self {
            EgressPolicy::LocalOnly => "local_only",
            EgressPolicy::OrgApproved => "org_approved",
            EgressPolicy::Any => "any",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "local_only" => Some(EgressPolicy::LocalOnly),
            "org_approved" => Some(EgressPolicy::OrgApproved),
            "any" => Some(EgressPolicy::Any),
            _ => None,
        }
    }
}

/// What the owner node can actually do about outbound traffic. Probed, never
/// assumed: `NodeCapabilities::probe` reports `false` for anything it cannot
/// positively confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeCapabilities {
    /// A container runtime whose socket answers on this node.
    pub container_runtime: bool,
    /// A firewall rule matching on the process owner (Linux `--uid-owner` /
    /// `skuid`, macOS PF `user`, Windows WFP), installed when the node was set
    /// up.
    pub uid_owner_firewall: bool,
}

impl NodeCapabilities {
    /// Reads the node. Both probes are real checks (a runtime socket that
    /// exists, a firewall ruleset that names the service account); anything
    /// undecidable — no permission to read the ruleset, an unparsable answer,
    /// a missing tool — reports `false`.
    pub fn probe() -> Self {
        NodeCapabilities {
            container_runtime: firewall::container_runtime_present(),
            uid_owner_firewall: firewall::uid_owner_firewall_present(),
        }
    }
}

/// Resolves `egress_enforcement` when a workspace is created (§7.6). The value
/// is a statement about the node, so it is computed once and stored — the UI
/// then says what the workspace really is instead of implying a guarantee.
pub fn detect_enforcement(exec_mode: ExecMode, caps: &NodeCapabilities) -> EgressEnforcement {
    match exec_mode {
        ExecMode::ProcessSandbox if super::process_sandbox::ProcessSandbox::check_available().is_ok() => EgressEnforcement::ProcessSandbox,
        ExecMode::Container if caps.container_runtime => EgressEnforcement::Namespace,
        ExecMode::TrustedNative if caps.uid_owner_firewall => EgressEnforcement::Firewall,
        _ => EgressEnforcement::Unrestricted,
    }
}

/// Server-side validation of the mode combination (§9.5). This runs in the
/// workspace-write and session-open handlers, NOT in the wizard: hiding an
/// option in the UI is not validation, because the binary protocol is reachable
/// without the UI. Every rejection below is a combination whose promise the node
/// cannot keep.
pub fn validate_policy(
    exec_mode: ExecMode,
    enforcement: EgressEnforcement,
    autonomy_ceiling: AutonomyMode,
    egress_policy: EgressPolicy,
    container_image: Option<&str>,
) -> Result<()> {
    if exec_mode == ExecMode::Container
        && container_image
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    {
        return Err(anyhow!("exec_mode 'container' requires container_image"));
    }

    // Unattended execution of arbitrary commands has no backstop but a list of
    // names once the process runs as the service user on the host itself.
    if exec_mode == ExecMode::TrustedNative && autonomy_ceiling == AutonomyMode::Autonomous {
        return Err(anyhow!(
            "exec_mode 'trusted_native' caps autonomy at 'auto_edit'; 'autonomous' needs a container"
        ));
    }

    if egress_policy == EgressPolicy::LocalOnly && enforcement == EgressEnforcement::Unrestricted {
        return Err(anyhow!(
            "egress_policy 'local_only' needs a container namespace or a uid-owner firewall rule; \
             this node enforces nothing"
        ));
    }

    // `namespace` means "a container without a default route". Claiming it for
    // a native workspace would describe a mechanism that does not exist.
    if enforcement == EgressEnforcement::Namespace && exec_mode == ExecMode::TrustedNative {
        return Err(anyhow!(
            "egress_enforcement 'namespace' requires exec_mode 'container'"
        ));
    }

    Ok(())
}

/// One destination pattern: `example.com`, `*.example.com`, `*`, an IP literal,
/// each optionally pinned to a port (`registry.internal:5000`). A pattern
/// without a port matches ONLY the default HTTP/HTTPS ports — otherwise
/// allowlisting a web host would silently open its SSH or database port too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPattern {
    host: String,
    port: Option<u16>,
}

impl HostPattern {
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.len() > 255 {
            return Err(anyhow!("empty or oversized host pattern"));
        }
        let (host, port) = split_host_port(raw)?;
        let host = normalize_host(&host);
        if host.is_empty() {
            return Err(anyhow!("host pattern has no host part"));
        }
        let body = host.strip_prefix("*.").unwrap_or(&host);
        if host != "*"
            && !body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
        {
            return Err(anyhow!("host pattern {raw} contains an unusable character"));
        }
        Ok(HostPattern { host, port })
    }

    /// Host part only. Used to tell "this host is not on the list" apart from
    /// "this host is, but not on that port" — two different denials.
    fn matches_host(&self, host: &str) -> bool {
        let host = normalize_host(host);
        if self.host == "*" {
            return true;
        }
        if let Some(suffix) = self.host.strip_prefix("*.") {
            // A wildcard covers subdomains, never the bare domain: `*.corp.example`
            // must not silently mean `corp.example`.
            return host != suffix
                && host
                    .strip_suffix(suffix)
                    .is_some_and(|prefix| prefix.ends_with('.'));
        }
        self.host == host
    }

    fn matches(&self, host: &str, port: u16) -> bool {
        if !self.matches_host(host) {
            return false;
        }
        match self.port {
            Some(pinned) => pinned == port,
            None => port == 80 || port == 443,
        }
    }
}

fn split_host_port(raw: &str) -> Result<(String, Option<u16>)> {
    if let Some(rest) = raw.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| anyhow!("unterminated ipv6 literal in {raw}"))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(p.parse::<u16>().map_err(|_| anyhow!("bad port in {raw}"))?),
            None if tail.is_empty() => None,
            None => return Err(anyhow!("trailing characters in {raw}")),
        };
        return Ok((host.to_string(), port));
    }
    match raw.rsplit_once(':') {
        // An unbracketed IPv6 literal has several colons; treat it as a host.
        Some((host, port)) if !host.contains(':') => Ok((
            host.to_string(),
            Some(
                port.parse::<u16>()
                    .map_err(|_| anyhow!("bad port in {raw}"))?,
            ),
        )),
        _ => Ok((raw.to_string(), None)),
    }
}

/// One host spelling for every layer. `remote_policy` matches its control-plane
/// name list against this, so a root dot (`metadata.google.internal.`) or an
/// upper-case letter cannot mean a different host here than it does in DNS.
pub(crate) fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressScheme {
    Http,
    Https,
}

impl EgressScheme {
    pub fn slug(self) -> &'static str {
        match self {
            EgressScheme::Http => "http",
            EgressScheme::Https => "https",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            EgressScheme::Http => 80,
            EgressScheme::Https => 443,
        }
    }
}

/// Which session and run a request belongs to, so the event names an actor
/// rather than just a workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressContext {
    pub session_id: Option<String>,
    pub run_id: Option<String>,
}

/// Shape of the request, kept in the event so a tunnel, a plain request and a
/// followed redirect are distinguishable in the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestKind {
    /// `CONNECT host:port` — the gateway sees the destination but not the body.
    Connect,
    Http {
        method: String,
    },
    /// Re-check of a `Location` the caller intends to follow.
    Redirect {
        from: String,
    },
}

impl RequestKind {
    fn slug(&self) -> &'static str {
        match self {
            RequestKind::Connect => "connect",
            RequestKind::Http { .. } => "http",
            RequestKind::Redirect { .. } => "redirect",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRequest {
    pub ctx: EgressContext,
    pub kind: RequestKind,
    pub scheme: EgressScheme,
    pub host: String,
    pub port: u16,
}

impl EgressRequest {
    /// Builds a request from a URL. A scheme other than HTTP/HTTPS is refused
    /// here, which is the whole reason `ssh://` and `git://` have no route.
    pub fn from_url(ctx: EgressContext, kind: RequestKind, raw: &str) -> Result<Self> {
        let url = Url::parse(raw).map_err(|e| anyhow!("invalid url: {e}"))?;
        let scheme = match url.scheme() {
            "http" => EgressScheme::Http,
            "https" => EgressScheme::Https,
            other => {
                return Err(anyhow!(
                    "protocol {other} is not routable through the gateway"
                ))
            }
        };
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("url has no host"))?
            .to_string();
        let port = url.port().unwrap_or_else(|| scheme.default_port());
        Ok(EgressRequest {
            ctx,
            kind,
            scheme,
            host,
            port,
        })
    }
}

/// A destination that passed every check, with the addresses PINNED. The
/// connector must use these and must not resolve the name again — re-resolving
/// is exactly the window a rebinding attack needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedRoute {
    pub ctx: EgressContext,
    pub scheme: EgressScheme,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<SocketAddr>,
    /// True when the destination is an approved local service of the owner node
    /// rather than a remote host.
    pub local_service: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressOutcome {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialReason {
    /// Anything that is not HTTP or HTTPS, including an absolute-form request
    /// the proxy cannot serve.
    ProtocolNotRoutable,
    /// Cloud metadata, cluster control plane, loopback outside an approved
    /// port, link-local, multicast.
    ForbiddenDestination,
    NotAllowlisted,
    PortNotAllowed,
    PolicyLocalOnly,
    Unresolvable,
    SniMismatch,
    ProxyAuth,
    MalformedRequest,
}

impl DenialReason {
    pub fn slug(self) -> &'static str {
        match self {
            DenialReason::ProtocolNotRoutable => "protocol_not_routable",
            DenialReason::ForbiddenDestination => "forbidden_destination",
            DenialReason::NotAllowlisted => "not_allowlisted",
            DenialReason::PortNotAllowed => "port_not_allowed",
            DenialReason::PolicyLocalOnly => "policy_local_only",
            DenialReason::Unresolvable => "unresolvable",
            DenialReason::SniMismatch => "sni_mismatch",
            DenialReason::ProxyAuth => "proxy_auth",
            DenialReason::MalformedRequest => "malformed_request",
        }
    }
}

/// One line of the egress journal, in the gateway's own vocabulary. It is
/// handed back to the caller, which writes it into the session timeline through
/// `events::append` — there is no second journal here, and therefore no
/// timestamp and no database handle in this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressEvent {
    pub workspace_id: String,
    pub ctx: EgressContext,
    pub outcome: EgressOutcome,
    pub request_kind: RequestKind,
    pub scheme: EgressScheme,
    pub host: String,
    pub port: u16,
    /// Addresses the gateway resolved and checked. Empty when the request was
    /// refused before resolution.
    pub addresses: Vec<IpAddr>,
    pub policy: EgressPolicy,
    pub enforcement: EgressEnforcement,
    pub denial: Option<DenialReason>,
    pub detail: Option<String>,
}

impl EgressEvent {
    /// Turns the decision into the timeline's own payload. The session log owns
    /// the sequence, the redaction and the audit mirror (`EventPayload::Egress`
    /// is security-relevant there, so every request AND every refusal reaches
    /// `audit_log`), and this is the single conversion into it — the gateway
    /// does not carry a second event vocabulary into the database.
    pub fn into_payload(self) -> EventPayload {
        let allowed = self.outcome == EgressOutcome::Allowed;
        let url = format!("{}://{}:{}", self.scheme.slug(), self.host, self.port);
        let reason = match self.denial {
            Some(denial) => format!(
                "{} denied [{}] {}",
                self.request_kind.slug(),
                denial.slug(),
                self.detail.unwrap_or_default()
            ),
            None => {
                let addresses: Vec<String> =
                    self.addresses.iter().map(|ip| ip.to_string()).collect();
                format!(
                    "{} allowed via {}",
                    self.request_kind.slug(),
                    addresses.join(",")
                )
            }
        };
        EventPayload::Egress {
            url,
            allowed,
            reason,
        }
    }
}

/// An approved destination plus the event that records it.
#[derive(Debug, Clone)]
pub struct Allowed {
    pub route: PinnedRoute,
    pub event: EgressEvent,
}

/// A refusal plus the event that records it. Both arms of the screening result
/// carry an event, so "every request and every refusal is an event" holds by
/// construction rather than by discipline.
#[derive(Debug, Clone)]
pub struct Denied {
    pub reason: DenialReason,
    pub detail: String,
    pub event: EgressEvent,
}

/// Everything the gateway needs. The lists come from the registry
/// (`code_workspace_allowlist` with capability `net_egress`) and the
/// organization provider list; the caller loads them, so the gateway itself
/// stays free of database access and is exhaustively testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressGatewayConfig {
    pub workspace_id: String,
    pub enforcement: EgressEnforcement,
    pub policy: EgressPolicy,
    pub workspace_allowlist: Vec<HostPattern>,
    pub org_approved: Vec<HostPattern>,
    /// Local services of the owner node the sandbox may use, as exact
    /// address+port pairs. This is the ONLY way loopback becomes reachable.
    pub local_services: Vec<SocketAddr>,
    /// Presented by the sandbox in `Proxy-Authorization`. In native mode the
    /// listener is reachable by every process on the host, so the token is what
    /// separates the workspace's traffic from a bystander's.
    pub proxy_token: String,
}

/// What a workspace got. `NoMechanism` is not an error path — it is the honest
/// answer for a node that enforces nothing, and the caller has to render or
/// refuse it rather than pretend a gateway exists.
pub enum WorkspaceEgress {
    Enforced(Arc<EgressGateway>),
    NoMechanism(NoMechanism),
}

/// Says plainly that nothing filters and no host is audited on this workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoMechanism {
    pub workspace_id: String,
    pub enforcement: EgressEnforcement,
}

impl NoMechanism {
    pub fn reason(&self) -> &'static str {
        "this node has neither a container namespace nor a uid-owner firewall rule: \
         outbound traffic is neither filtered nor audited"
    }
}

impl WorkspaceEgress {
    /// `Some` only when a mechanism exists. A caller that wants to claim
    /// filtering has to go through here.
    pub fn gateway(&self) -> Option<&Arc<EgressGateway>> {
        match self {
            WorkspaceEgress::Enforced(gateway) => Some(gateway),
            WorkspaceEgress::NoMechanism(_) => None,
        }
    }

    pub fn filters(&self) -> bool {
        matches!(self, WorkspaceEgress::Enforced(_))
    }
}

pub struct EgressGateway {
    config: EgressGatewayConfig,
    resolver: Arc<dyn Resolver>,
}

impl EgressGateway {
    /// Builds the gateway for a workspace, or states that this node has no
    /// mechanism. `unrestricted` never yields a gateway: an object that screens
    /// requests nobody is forced to send would report a filtered world while the
    /// sandbox talks to the internet directly.
    pub fn for_workspace(
        config: EgressGatewayConfig,
        resolver: Arc<dyn Resolver>,
    ) -> WorkspaceEgress {
        let enforcement = config.enforcement;
        match enforcement {
            EgressEnforcement::Namespace | EgressEnforcement::Firewall | EgressEnforcement::ProcessSandbox => {
                WorkspaceEgress::Enforced(Arc::new(EgressGateway { config, resolver }))
            }
            EgressEnforcement::Unrestricted => WorkspaceEgress::NoMechanism(NoMechanism {
                workspace_id: config.workspace_id,
                enforcement: EgressEnforcement::Unrestricted,
            }),
        }
    }

    pub fn workspace_id(&self) -> &str {
        &self.config.workspace_id
    }

    pub fn policy(&self) -> EgressPolicy {
        self.config.policy
    }

    pub fn enforcement(&self) -> EgressEnforcement {
        self.config.enforcement
    }

    /// Constant-time check of the proxy credential presented by the sandbox.
    pub fn token_matches(&self, presented: &str) -> bool {
        use subtle::ConstantTimeEq;
        presented.len() == self.config.proxy_token.len()
            && bool::from(
                presented
                    .as_bytes()
                    .ct_eq(self.config.proxy_token.as_bytes()),
            )
    }

    /// The whole decision: name blocklist, policy, allowlist, port, DNS
    /// resolution, and every resolved address. On success the addresses are
    /// pinned for the operation.
    pub fn screen(&self, req: &EgressRequest) -> Result<Allowed, Denied> {
        let host = normalize_host(&req.host);
        if host.is_empty() {
            return Err(self.deny(req, &[], DenialReason::MalformedRequest, "empty host"));
        }
        // §11.4 blocks loopback "outside an approved port", not loopback as
        // such: the owner node's own services are how a workspace reaches its
        // sidecars at all. The exemption is computed first and is deliberately
        // narrow — only an operator-configured loopback socket qualifies, so a
        // metadata or control-plane address listed by mistake still falls
        // through to the ban below.
        let local_by_name = self.local_service_by_name(&host, req.port);
        let exempt_loopback = local_by_name
            && host
                .parse::<IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(host == "localhost");

        if !exempt_loopback && is_forbidden_host_literal(&host) {
            return Err(self.deny(
                req,
                &[],
                DenialReason::ForbiddenDestination,
                format!("{host} is a metadata or control-plane address"),
            ));
        }

        if let Some(denial) = self.name_gate(req, &host, local_by_name) {
            return Err(denial);
        }

        let addresses = match self.resolver.resolve(&host, req.port) {
            Ok(addresses) if !addresses.is_empty() => addresses,
            Ok(_) => {
                return Err(self.deny(
                    req,
                    &[],
                    DenialReason::Unresolvable,
                    format!("{host} resolved to no address"),
                ))
            }
            Err(error) => {
                return Err(self.deny(
                    req,
                    &[],
                    DenialReason::Unresolvable,
                    format!("cannot resolve {host}: {error:#}"),
                ))
            }
        };

        // Every answer is checked, not the first one: a name that returns one
        // public and one forbidden address must not pass on ordering, and the
        // answer to a second lookup is a different answer that gets its own
        // check.
        for addr in &addresses {
            let verdict = if local_by_name {
                // The name resolved into the local-service exemption, so the
                // ADDRESS has to be one of the approved services too — this is
                // where `localhost` rebound to something else is caught.
                self.config
                    .local_services
                    .iter()
                    .any(|approved| approved == addr)
            } else {
                !is_forbidden_ip(addr.ip())
            };
            if !verdict {
                let checked: Vec<IpAddr> = addresses.iter().map(|a| a.ip()).collect();
                return Err(self.deny(
                    req,
                    &checked,
                    DenialReason::ForbiddenDestination,
                    format!("{host} resolves to {}", addr.ip()),
                ));
            }
        }

        let checked: Vec<IpAddr> = addresses.iter().map(|a| a.ip()).collect();
        Ok(Allowed {
            route: PinnedRoute {
                ctx: req.ctx.clone(),
                scheme: req.scheme,
                host,
                port: req.port,
                addresses,
                local_service: local_by_name,
            },
            event: self.event(req, &checked, None, None),
        })
    }

    /// Re-checks a `Location` before the caller follows it. A redirect is a new
    /// destination chosen by the remote side, so it goes through the identical
    /// gate — including a fresh resolution, because the redirect target may
    /// share the name but not the address. The proxy itself never follows
    /// redirects; this exists for the components that do.
    pub fn screen_redirect(&self, from: &PinnedRoute, location: &str) -> Result<Allowed, Denied> {
        let kind = RequestKind::Redirect {
            from: format!("{}://{}:{}", from.scheme.slug(), from.host, from.port),
        };
        let request = match Url::parse(location) {
            Ok(url) => {
                let scheme = match url.scheme() {
                    "http" => EgressScheme::Http,
                    "https" => EgressScheme::Https,
                    other => {
                        let req = EgressRequest {
                            ctx: from.ctx.clone(),
                            kind,
                            scheme: from.scheme,
                            host: from.host.clone(),
                            port: from.port,
                        };
                        return Err(self.deny(
                            &req,
                            &[],
                            DenialReason::ProtocolNotRoutable,
                            format!("redirect to {other} is not routable"),
                        ));
                    }
                };
                let host = match url.host_str() {
                    Some(host) => host.to_string(),
                    None => {
                        let req = EgressRequest {
                            ctx: from.ctx.clone(),
                            kind,
                            scheme: from.scheme,
                            host: from.host.clone(),
                            port: from.port,
                        };
                        return Err(self.deny(
                            &req,
                            &[],
                            DenialReason::MalformedRequest,
                            "redirect target has no host".to_string(),
                        ));
                    }
                };
                EgressRequest {
                    ctx: from.ctx.clone(),
                    kind,
                    scheme,
                    host,
                    port: url.port().unwrap_or_else(|| scheme.default_port()),
                }
            }
            // A relative `Location` keeps the current destination, but the
            // addresses are still resolved and checked again.
            Err(_) => EgressRequest {
                ctx: from.ctx.clone(),
                kind,
                scheme: from.scheme,
                host: from.host.clone(),
                port: from.port,
            },
        };
        self.screen(&request)
    }

    /// The name in the TLS ClientHello must be the name the `CONNECT` asked
    /// for. Without this the client could ask for an allowlisted host and then
    /// negotiate a different one on the same tunnel; a missing SNI is refused
    /// too, because an unverifiable tunnel is not a verified one.
    pub fn verify_sni(&self, route: &PinnedRoute, sni: Option<&str>) -> Result<(), Denied> {
        let req = EgressRequest {
            ctx: route.ctx.clone(),
            kind: RequestKind::Connect,
            scheme: route.scheme,
            host: route.host.clone(),
            port: route.port,
        };
        let checked: Vec<IpAddr> = route.addresses.iter().map(|a| a.ip()).collect();
        match sni {
            Some(name) if normalize_host(name) == route.host => Ok(()),
            Some(name) => Err(self.deny(
                &req,
                &checked,
                DenialReason::SniMismatch,
                format!(
                    "connect asked for {} but the handshake offered {name}",
                    route.host
                ),
            )),
            None => Err(self.deny(
                &req,
                &checked,
                DenialReason::SniMismatch,
                "the handshake carried no server name".to_string(),
            )),
        }
    }

    /// Builds a refusal for something the gateway rejected before it could
    /// become a destination — an unparsable request line, a missing proxy
    /// credential. Public because the proxy front end meets those cases first
    /// and they are events like any other.
    pub fn refuse(
        &self,
        req: &EgressRequest,
        reason: DenialReason,
        detail: impl Into<String>,
    ) -> Denied {
        self.deny(req, &[], reason, detail)
    }

    /// Policy and allowlist, before any DNS query is sent for a host that is
    /// not going to be permitted anyway.
    fn name_gate(&self, req: &EgressRequest, host: &str, local: bool) -> Option<Denied> {
        if local {
            return None;
        }
        match self.config.policy {
            EgressPolicy::LocalOnly => Some(self.deny(
                req,
                &[],
                DenialReason::PolicyLocalOnly,
                format!("{host} is not a local service of the owner node"),
            )),
            EgressPolicy::OrgApproved => {
                if let Some(denial) =
                    self.allowlist_gate(req, host, &self.config.workspace_allowlist)
                {
                    return Some(denial);
                }
                self.allowlist_gate(req, host, &self.config.org_approved)
            }
            EgressPolicy::Any => self.allowlist_gate(req, host, &self.config.workspace_allowlist),
        }
    }

    fn allowlist_gate(
        &self,
        req: &EgressRequest,
        host: &str,
        patterns: &[HostPattern],
    ) -> Option<Denied> {
        if patterns.iter().any(|p| p.matches(host, req.port)) {
            return None;
        }
        let reason = if patterns.iter().any(|p| p.matches_host(host)) {
            DenialReason::PortNotAllowed
        } else {
            DenialReason::NotAllowlisted
        };
        Some(self.deny(
            req,
            &[],
            reason,
            format!("{host}:{} is outside the allowlist", req.port),
        ))
    }

    /// Whether the requested name is one of the approved local services. Only
    /// an IP literal or `localhost` can be one — a public name that happens to
    /// resolve to loopback is a rebinding attempt, not a local service, and is
    /// caught by the per-address check.
    fn local_service_by_name(&self, host: &str, port: u16) -> bool {
        if host == "localhost" {
            return self
                .config
                .local_services
                .iter()
                .any(|addr| addr.ip().is_loopback() && addr.port() == port);
        }
        match host.parse::<IpAddr>() {
            Ok(ip) => self
                .config
                .local_services
                .iter()
                .any(|addr| addr.ip() == ip && addr.port() == port),
            Err(_) => false,
        }
    }

    fn event(
        &self,
        req: &EgressRequest,
        addresses: &[IpAddr],
        denial: Option<DenialReason>,
        detail: Option<String>,
    ) -> EgressEvent {
        EgressEvent {
            workspace_id: self.config.workspace_id.clone(),
            ctx: req.ctx.clone(),
            outcome: if denial.is_some() {
                EgressOutcome::Denied
            } else {
                EgressOutcome::Allowed
            },
            request_kind: req.kind.clone(),
            scheme: req.scheme,
            host: req.host.clone(),
            port: req.port,
            addresses: addresses.to_vec(),
            policy: self.config.policy,
            enforcement: self.config.enforcement,
            denial,
            detail,
        }
    }

    fn deny(
        &self,
        req: &EgressRequest,
        addresses: &[IpAddr],
        reason: DenialReason,
        detail: impl Into<String>,
    ) -> Denied {
        let detail = detail.into();
        Denied {
            reason,
            detail: detail.clone(),
            event: self.event(req, addresses, Some(reason), Some(detail)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resolver::Resolver;
    use super::*;
    use std::sync::Mutex;

    /// Answers with a fixed set. Injected so no test depends on DNS.
    struct StaticResolver(Vec<SocketAddr>);

    impl Resolver for StaticResolver {
        fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>> {
            Ok(self.0.clone())
        }
    }

    /// Answers differently on every call — the shape of a rebinding attack:
    /// the name is public when it is checked and loopback a moment later.
    struct SequenceResolver {
        answers: Mutex<Vec<Vec<SocketAddr>>>,
        calls: Mutex<usize>,
    }

    impl SequenceResolver {
        fn new(answers: Vec<Vec<SocketAddr>>) -> Self {
            SequenceResolver {
                answers: Mutex::new(answers),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl Resolver for SequenceResolver {
        fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>> {
            let mut calls = self.calls.lock().unwrap();
            let answers = self.answers.lock().unwrap();
            let answer = answers
                .get(*calls)
                .cloned()
                .unwrap_or_else(|| answers.last().cloned().unwrap_or_default());
            *calls += 1;
            Ok(answer)
        }
    }

    fn patterns(raw: &[&str]) -> Vec<HostPattern> {
        raw.iter().map(|p| HostPattern::parse(p).unwrap()).collect()
    }

    fn config(policy: EgressPolicy) -> EgressGatewayConfig {
        EgressGatewayConfig {
            workspace_id: "ws-1".into(),
            enforcement: EgressEnforcement::Namespace,
            policy,
            workspace_allowlist: patterns(&["crates.io", "*.crates.io", "registry.lan:5000"]),
            org_approved: patterns(&["crates.io", "*.crates.io", "registry.lan:5000"]),
            local_services: vec!["127.0.0.1:8093".parse().unwrap()],
            proxy_token: "s3cret-token".into(),
        }
    }

    fn gateway_with(policy: EgressPolicy, resolver: Arc<dyn Resolver>) -> Arc<EgressGateway> {
        match EgressGateway::for_workspace(config(policy), resolver) {
            WorkspaceEgress::Enforced(gateway) => gateway,
            WorkspaceEgress::NoMechanism(_) => panic!("expected an enforcing gateway"),
        }
    }

    fn gateway(policy: EgressPolicy) -> Arc<EgressGateway> {
        gateway_with(
            policy,
            Arc::new(StaticResolver(vec!["93.184.216.34:443".parse().unwrap()])),
        )
    }

    fn request(host: &str, port: u16) -> EgressRequest {
        EgressRequest {
            ctx: EgressContext {
                session_id: Some("sess-1".into()),
                run_id: Some("run-1".into()),
            },
            kind: RequestKind::Connect,
            scheme: EgressScheme::Https,
            host: host.to_string(),
            port,
        }
    }

    #[test]
    fn a_host_outside_the_allowlist_is_refused_and_the_refusal_is_an_event() {
        let gateway = gateway(EgressPolicy::Any);
        let denied = gateway.screen(&request("evil.example", 443)).unwrap_err();
        assert_eq!(denied.reason, DenialReason::NotAllowlisted);
        assert_eq!(denied.event.outcome, EgressOutcome::Denied);
        assert_eq!(denied.event.host, "evil.example");

        // The refusal becomes a timeline event, which is what carries it into
        // the audit mirror — nothing here writes a journal of its own.
        match denied.event.into_payload() {
            EventPayload::Egress {
                url,
                allowed,
                reason,
            } => {
                assert_eq!(url, "https://evil.example:443");
                assert!(!allowed);
                assert!(reason.contains("not_allowlisted"), "{reason}");
            }
            other => panic!("an egress refusal became {other:?}"),
        }

        // The allowlisted one passes and is an event too.
        let allowed = gateway.screen(&request("crates.io", 443)).unwrap();
        assert_eq!(allowed.event.outcome, EgressOutcome::Allowed);
        assert_eq!(allowed.route.addresses.len(), 1);
        match allowed.event.into_payload() {
            EventPayload::Egress {
                allowed, reason, ..
            } => {
                assert!(allowed);
                assert!(reason.contains("93.184.216.34"), "{reason}");
            }
            other => panic!("an allowed request became {other:?}"),
        }
    }

    #[test]
    fn an_allowlisted_host_on_an_unlisted_port_is_a_different_refusal() {
        let gateway = gateway(EgressPolicy::Any);
        let denied = gateway.screen(&request("crates.io", 22)).unwrap_err();
        assert_eq!(denied.reason, DenialReason::PortNotAllowed);

        // The pattern that pins a port opens exactly that port.
        let gateway = gateway_with(
            EgressPolicy::Any,
            Arc::new(StaticResolver(vec!["10.1.2.3:5000".parse().unwrap()])),
        );
        assert!(gateway.screen(&request("registry.lan", 5000)).is_ok());
        assert_eq!(
            gateway
                .screen(&request("registry.lan", 443))
                .unwrap_err()
                .reason,
            DenialReason::PortNotAllowed
        );
    }

    #[test]
    fn dns_rebinding_does_not_get_past_the_check() {
        // First answer public, second loopback. Both answers are screened; the
        // route the gateway hands back carries the address it checked, so the
        // connector never re-resolves.
        let resolver = Arc::new(SequenceResolver::new(vec![
            vec!["93.184.216.34:443".parse().unwrap()],
            vec!["127.0.0.1:443".parse().unwrap()],
        ]));
        let gateway = gateway_with(EgressPolicy::Any, resolver.clone());

        let allowed = gateway.screen(&request("crates.io", 443)).unwrap();
        assert_eq!(
            allowed.route.addresses,
            vec!["93.184.216.34:443".parse::<SocketAddr>().unwrap()]
        );

        let denied = gateway.screen(&request("crates.io", 443)).unwrap_err();
        assert_eq!(denied.reason, DenialReason::ForbiddenDestination);
        assert_eq!(
            denied.event.addresses,
            vec!["127.0.0.1".parse::<IpAddr>().unwrap()]
        );
        assert_eq!(resolver.calls(), 2, "both answers went through the check");
    }

    #[test]
    fn one_bad_address_in_a_multi_answer_fails_the_whole_destination() {
        let gateway = gateway_with(
            EgressPolicy::Any,
            Arc::new(StaticResolver(vec![
                "93.184.216.34:443".parse().unwrap(),
                "169.254.169.254:443".parse().unwrap(),
            ])),
        );
        let denied = gateway.screen(&request("crates.io", 443)).unwrap_err();
        assert_eq!(denied.reason, DenialReason::ForbiddenDestination);
    }

    #[test]
    fn cloud_metadata_is_blocked_by_name_and_by_address_in_both_families() {
        let gateway = gateway_with(
            EgressPolicy::Any,
            Arc::new(StaticResolver(vec!["93.184.216.34:443".parse().unwrap()])),
        );
        for host in [
            "169.254.169.254",
            "fd00:ec2::254",
            "[fd00:ec2::254]",
            "metadata.google.internal",
            "kubernetes.default.svc",
        ] {
            let denied = gateway.screen(&request(host, 443)).unwrap_err();
            assert_eq!(
                denied.reason,
                DenialReason::ForbiddenDestination,
                "{host} was not blocked as a forbidden destination"
            );
        }
    }

    #[test]
    fn a_public_name_that_resolves_to_metadata_is_blocked_after_resolution() {
        let gateway = gateway_with(
            EgressPolicy::Any,
            Arc::new(StaticResolver(vec!["169.254.169.254:443".parse().unwrap()])),
        );
        let denied = gateway.screen(&request("crates.io", 443)).unwrap_err();
        assert_eq!(denied.reason, DenialReason::ForbiddenDestination);
        assert!(denied.detail.contains("169.254.169.254"));
    }

    #[test]
    fn a_redirect_out_of_the_allowlist_is_refused_and_the_target_is_resolved_again() {
        let resolver = Arc::new(SequenceResolver::new(vec![
            vec!["93.184.216.34:443".parse().unwrap()],
            vec!["127.0.0.1:443".parse().unwrap()],
        ]));
        let gateway = gateway_with(EgressPolicy::Any, resolver.clone());
        let allowed = gateway.screen(&request("crates.io", 443)).unwrap();

        let denied = gateway
            .screen_redirect(&allowed.route, "https://evil.example/take-this")
            .unwrap_err();
        assert_eq!(denied.reason, DenialReason::NotAllowlisted);
        assert!(matches!(
            denied.event.request_kind,
            RequestKind::Redirect { .. }
        ));

        // A redirect that stays on the allowlist is still re-resolved, so a
        // name that has meanwhile moved to loopback is caught.
        let denied = gateway
            .screen_redirect(&allowed.route, "/moved-here")
            .unwrap_err();
        assert_eq!(denied.reason, DenialReason::ForbiddenDestination);
        assert_eq!(resolver.calls(), 2);
    }

    #[test]
    fn a_redirect_to_another_protocol_has_nowhere_to_go() {
        let gateway = gateway(EgressPolicy::Any);
        let allowed = gateway.screen(&request("crates.io", 443)).unwrap();
        for location in [
            "ssh://crates.io/x",
            "git://crates.io/x",
            "file:///etc/passwd",
        ] {
            let denied = gateway
                .screen_redirect(&allowed.route, location)
                .unwrap_err();
            assert_eq!(
                denied.reason,
                DenialReason::ProtocolNotRoutable,
                "{location} found a route"
            );
        }
    }

    #[test]
    fn a_connect_tunnel_must_negotiate_the_name_it_asked_for() {
        let gateway = gateway(EgressPolicy::Any);
        let allowed = gateway.screen(&request("crates.io", 443)).unwrap();

        assert!(gateway
            .verify_sni(&allowed.route, Some("crates.io"))
            .is_ok());
        assert!(gateway
            .verify_sni(&allowed.route, Some("CRATES.IO."))
            .is_ok());

        let denied = gateway
            .verify_sni(&allowed.route, Some("evil.example"))
            .unwrap_err();
        assert_eq!(denied.reason, DenialReason::SniMismatch);
        assert_eq!(denied.event.outcome, EgressOutcome::Denied);

        // No SNI at all is not "nothing to check", it is unverifiable.
        let denied = gateway.verify_sni(&allowed.route, None).unwrap_err();
        assert_eq!(denied.reason, DenialReason::SniMismatch);
    }

    #[test]
    fn local_only_really_cuts_the_network() {
        let gateway = gateway_with(
            EgressPolicy::LocalOnly,
            Arc::new(StaticResolver(vec!["127.0.0.1:8093".parse().unwrap()])),
        );
        // Even an allowlisted host is refused: the policy is not the allowlist.
        for host in ["crates.io", "index.crates.io", "registry.lan"] {
            let denied = gateway.screen(&request(host, 443)).unwrap_err();
            assert_eq!(
                denied.reason,
                DenialReason::PolicyLocalOnly,
                "{host} reached the network under local_only"
            );
        }
        // The approved local service is exactly what stays reachable.
        let mut req = request("127.0.0.1", 8093);
        req.scheme = EgressScheme::Http;
        let allowed = gateway.screen(&req).unwrap();
        assert!(allowed.route.local_service);
    }

    #[test]
    fn loopback_is_reachable_only_on_the_approved_port_and_only_by_a_local_name() {
        let gateway = gateway_with(
            EgressPolicy::Any,
            Arc::new(StaticResolver(vec!["127.0.0.1:9999".parse().unwrap()])),
        );
        // Same host, unapproved port: not a local service, so the ordinary
        // loopback ban applies. §11.4 lists "loopback outside an approved port"
        // among the ALWAYS-blocked destinations, so this is a forbidden
        // destination rather than a missing allowlist entry — the distinction
        // matters because it is what the audit trail records.
        let denied = gateway.screen(&request("127.0.0.1", 9999)).unwrap_err();
        assert_eq!(denied.reason, DenialReason::ForbiddenDestination);

        // `localhost` rebound away from the approved service is refused.
        let gateway = gateway_with(
            EgressPolicy::Any,
            Arc::new(StaticResolver(vec!["127.0.0.1:8093".parse().unwrap()])),
        );
        let mut req = request("localhost", 8093);
        req.scheme = EgressScheme::Http;
        assert!(gateway.screen(&req).is_ok());

        let gateway = gateway_with(
            EgressPolicy::Any,
            Arc::new(StaticResolver(vec!["10.0.0.9:8093".parse().unwrap()])),
        );
        let denied = gateway.screen(&req).unwrap_err();
        assert_eq!(denied.reason, DenialReason::ForbiddenDestination);
    }

    #[test]
    fn org_approved_needs_both_lists() {
        let mut cfg = config(EgressPolicy::OrgApproved);
        cfg.workspace_allowlist = patterns(&["crates.io", "internal.example"]);
        cfg.org_approved = patterns(&["crates.io"]);
        let gateway = match EgressGateway::for_workspace(
            cfg,
            Arc::new(StaticResolver(vec!["93.184.216.34:443".parse().unwrap()])),
        ) {
            WorkspaceEgress::Enforced(gateway) => gateway,
            WorkspaceEgress::NoMechanism(_) => panic!("expected a gateway"),
        };
        assert!(gateway.screen(&request("crates.io", 443)).is_ok());
        assert_eq!(
            gateway
                .screen(&request("internal.example", 443))
                .unwrap_err()
                .reason,
            DenialReason::NotAllowlisted
        );
    }

    #[test]
    fn an_unrestricted_node_gets_no_gateway_that_pretends_to_filter() {
        let mut cfg = config(EgressPolicy::Any);
        cfg.enforcement = EgressEnforcement::Unrestricted;
        let egress = EgressGateway::for_workspace(
            cfg,
            Arc::new(StaticResolver(vec!["93.184.216.34:443".parse().unwrap()])),
        );
        assert!(!egress.filters());
        assert!(egress.gateway().is_none());
        match egress {
            WorkspaceEgress::NoMechanism(no) => {
                assert_eq!(no.enforcement, EgressEnforcement::Unrestricted);
                assert!(no.reason().contains("neither filtered nor audited"));
            }
            WorkspaceEgress::Enforced(_) => panic!("unrestricted must not produce a gateway"),
        }
    }

    #[test]
    fn non_http_protocols_never_become_a_request() {
        for raw in [
            "ssh://git@github.com/org/repo.git",
            "git://github.com/org/repo.git",
            "file:///etc/passwd",
            "ftp://example.invalid/x",
        ] {
            assert!(
                EgressRequest::from_url(
                    EgressContext::default(),
                    RequestKind::Http {
                        method: "GET".into()
                    },
                    raw
                )
                .is_err(),
                "{raw} produced a routable request"
            );
        }
        assert!(EgressRequest::from_url(
            EgressContext::default(),
            RequestKind::Http {
                method: "GET".into()
            },
            "https://crates.io/api"
        )
        .is_ok());
    }

    #[test]
    fn the_proxy_token_is_compared_whole() {
        let gateway = gateway(EgressPolicy::Any);
        assert!(gateway.token_matches("s3cret-token"));
        assert!(!gateway.token_matches("s3cret-toke"));
        assert!(!gateway.token_matches("s3cret-token "));
        assert!(!gateway.token_matches(""));
    }

    #[test]
    fn enforcement_follows_what_the_node_can_actually_do() {
        let both = NodeCapabilities {
            container_runtime: true,
            uid_owner_firewall: true,
        };
        let neither = NodeCapabilities {
            container_runtime: false,
            uid_owner_firewall: false,
        };
        assert_eq!(
            detect_enforcement(ExecMode::Container, &both),
            EgressEnforcement::Namespace
        );
        assert_eq!(
            detect_enforcement(ExecMode::TrustedNative, &both),
            EgressEnforcement::Firewall
        );
        // No mechanism is reported as none, never as the mode that was asked for.
        assert_eq!(
            detect_enforcement(ExecMode::Container, &neither),
            EgressEnforcement::Unrestricted
        );
        assert_eq!(
            detect_enforcement(ExecMode::TrustedNative, &neither),
            EgressEnforcement::Unrestricted
        );
        assert_eq!(
            detect_enforcement(
                ExecMode::TrustedNative,
                &NodeCapabilities {
                    container_runtime: true,
                    uid_owner_firewall: false
                }
            ),
            EgressEnforcement::Unrestricted
        );
    }

    #[test]
    fn the_server_refuses_native_autonomous_whatever_the_ui_showed() {
        let err = validate_policy(
            ExecMode::TrustedNative,
            EgressEnforcement::Firewall,
            AutonomyMode::Autonomous,
            EgressPolicy::OrgApproved,
            None,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("auto_edit"));

        assert!(validate_policy(
            ExecMode::TrustedNative,
            EgressEnforcement::Firewall,
            AutonomyMode::AutoEdit,
            EgressPolicy::OrgApproved,
            None,
        )
        .is_ok());
        assert!(validate_policy(
            ExecMode::Container,
            EgressEnforcement::Namespace,
            AutonomyMode::Autonomous,
            EgressPolicy::OrgApproved,
            Some("registry.lan/img:1"),
        )
        .is_ok());
    }

    #[test]
    fn the_server_refuses_local_only_without_a_mechanism() {
        let err = validate_policy(
            ExecMode::TrustedNative,
            EgressEnforcement::Unrestricted,
            AutonomyMode::Normal,
            EgressPolicy::LocalOnly,
            None,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("local_only"));

        assert!(validate_policy(
            ExecMode::TrustedNative,
            EgressEnforcement::Firewall,
            AutonomyMode::Normal,
            EgressPolicy::LocalOnly,
            None,
        )
        .is_ok());
    }

    #[test]
    fn the_server_refuses_a_container_without_an_image() {
        for image in [None, Some(""), Some("   ")] {
            assert!(
                validate_policy(
                    ExecMode::Container,
                    EgressEnforcement::Namespace,
                    AutonomyMode::Normal,
                    EgressPolicy::OrgApproved,
                    image,
                )
                .is_err(),
                "{image:?} was accepted as a container image"
            );
        }
    }

    #[test]
    fn a_namespace_claim_needs_a_container() {
        assert!(validate_policy(
            ExecMode::TrustedNative,
            EgressEnforcement::Namespace,
            AutonomyMode::Normal,
            EgressPolicy::OrgApproved,
            None,
        )
        .is_err());
    }

    #[test]
    fn a_wildcard_pattern_covers_subdomains_and_not_the_bare_domain() {
        let pattern = HostPattern::parse("*.crates.io").unwrap();
        assert!(pattern.matches("index.crates.io", 443));
        assert!(pattern.matches("a.b.crates.io", 443));
        assert!(!pattern.matches("crates.io", 443));
        assert!(!pattern.matches("evilcrates.io", 443));
        // No port in the pattern means the default ports only.
        assert!(!pattern.matches("index.crates.io", 8443));

        let pinned = HostPattern::parse("registry.lan:5000").unwrap();
        assert!(pinned.matches("registry.lan", 5000));
        assert!(!pinned.matches("registry.lan", 443));

        assert!(HostPattern::parse("[fd00::1]:8443").is_ok());
        assert!(HostPattern::parse("").is_err());
        assert!(HostPattern::parse("bad host").is_err());
    }
}
