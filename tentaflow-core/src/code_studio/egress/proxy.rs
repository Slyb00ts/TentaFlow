// ===== File: code_studio/egress/proxy.rs — the listener the sandbox's only route leads to =====
//
// An explicit HTTP/HTTPS proxy, not a NAT. The sandbox has no default route and
// no resolver; `HTTP_PROXY`/`HTTPS_PROXY` point here, so a destination that this
// file does not approve has no path out of the workspace at all.
//
// Three properties are structural rather than optional:
//
//   * Every request is screened. The proxy answers `Connection: close`, so a
//     connection carries exactly one request and no second destination can ride
//     a keep-alive socket that was approved for the first.
//   * Redirects are never followed here. The proxy relays the 3xx to the client;
//     if the client follows it, that is a NEW request through the same gate.
//     Components that follow redirects themselves call
//     `EgressGateway::screen_redirect` before they do.
//   * Anything that is not an HTTP request or a `CONNECT` is refused at the
//     first bytes. An `ssh` client speaking its banner into this port gets a
//     closed connection and an event — there is no tunnel to take.
//
// The listener is a TCP socket because the sandbox reaches it from its own
// network namespace, where a unix socket of the host is not visible. In native
// mode that socket is reachable by every process on the host, which is why the
// proxy credential is mandatory rather than advisory (§7.6).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tracing::{debug, warn};
use url::Url;

use super::sni::{scan_client_hello, SniScan};
use super::{
    Allowed, DenialReason, Denied, EgressContext, EgressEvent, EgressGateway, EgressRequest,
    EgressScheme, PinnedRoute, RequestKind,
};

/// Where screening events go. The proxy never touches a database — the caller
/// owns the session event log and decides how an event is persisted.
pub trait EgressEventSink: Send + Sync + 'static {
    fn record(&self, event: EgressEvent);
}

const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_CLIENT_HELLO_BYTES: usize = 16 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct EgressProxy {
    gateway: Arc<EgressGateway>,
    sink: Arc<dyn EgressEventSink>,
    listener: TcpListener,
}

impl EgressProxy {
    /// Binds the listener. The address is the caller's decision: the container
    /// bridge address for `namespace`, loopback for `firewall`.
    pub async fn bind(
        gateway: Arc<EgressGateway>,
        sink: Arc<dyn EgressEventSink>,
        addr: SocketAddr,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind egress gateway on {addr}"))?;
        Ok(EgressProxy {
            gateway,
            sink,
            listener,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.listener
            .local_addr()
            .context("read egress gateway address")
    }

    /// Accept loop. One task per connection; a failing connection never takes
    /// the listener down, because losing the gateway would leave the sandbox
    /// with no route rather than with a free one.
    pub async fn run(self) {
        loop {
            match self.listener.accept().await {
                Ok((stream, peer)) => {
                    let gateway = self.gateway.clone();
                    let sink = self.sink.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(stream, gateway, sink).await {
                            debug!(%peer, "egress gateway connection ended: {error:#}");
                        }
                    });
                }
                Err(error) => {
                    warn!("egress gateway accept failed: {error:#}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }
}

struct RequestHead {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    /// Bytes that arrived after the header block — the beginning of the body.
    rest: Vec<u8>,
}

impl RequestHead {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

async fn handle_connection(
    mut client: TcpStream,
    gateway: Arc<EgressGateway>,
    sink: Arc<dyn EgressEventSink>,
) -> Result<()> {
    // Bound to a local first: the read borrows the client, and holding that
    // borrow across the arms would block the refusal we write on it.
    let head_read = timeout(HEAD_TIMEOUT, read_head(&mut client)).await;
    let head = match head_read {
        Ok(Ok(head)) => head,
        Ok(Err(error)) => {
            // A client that never sends a valid request line is still an event:
            // it is how a non-HTTP protocol arrives at this port.
            refuse_unparsed(
                &mut client,
                &gateway,
                &sink,
                DenialReason::MalformedRequest,
                format!("{error:#}"),
                "400 Bad Request",
            )
            .await;
            return Ok(());
        }
        Err(_) => return Ok(()),
    };

    // The event names the WORKSPACE, not a session: everything the sandbox
    // sends is attacker-controlled, so a session id taken from a request header
    // would be attribution the gateway cannot stand behind. Per-actor
    // attribution needs a per-run credential, which the provider adapter has
    // (§7.5) and this listener does not.
    if !credential_matches(&gateway, &head) {
        let request = EgressRequest {
            ctx: EgressContext::default(),
            kind: RequestKind::Http {
                method: head.method.clone(),
            },
            scheme: EgressScheme::Http,
            host: head.target.clone(),
            port: 0,
        };
        let denied = gateway.refuse(
            &request,
            DenialReason::ProxyAuth,
            "the request carried no valid proxy credential",
        );
        sink.record(denied.event.clone());
        let _ = client
            .write_all(
                b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                  Proxy-Authenticate: Basic realm=\"tentaflow-code-studio\"\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await;
        return Ok(());
    }

    if head.method.eq_ignore_ascii_case("CONNECT") {
        handle_connect(client, head, gateway, sink).await
    } else {
        handle_plain(client, head, gateway, sink).await
    }
}

async fn handle_connect(
    mut client: TcpStream,
    head: RequestHead,
    gateway: Arc<EgressGateway>,
    sink: Arc<dyn EgressEventSink>,
) -> Result<()> {
    let (host, port) = match split_authority(&head.target, 443) {
        Some(pair) => pair,
        None => {
            let request = EgressRequest {
                ctx: EgressContext::default(),
                kind: RequestKind::Connect,
                scheme: EgressScheme::Https,
                host: head.target.clone(),
                port: 0,
            };
            let denied = gateway.refuse(
                &request,
                DenialReason::MalformedRequest,
                format!("connect target {} is not host:port", head.target),
            );
            deny_connection(&mut client, &sink, denied, "400 Bad Request").await;
            return Ok(());
        }
    };

    let request = EgressRequest {
        ctx: EgressContext::default(),
        kind: RequestKind::Connect,
        scheme: EgressScheme::Https,
        host,
        port,
    };
    let allowed = match screen(&gateway, request).await {
        Ok(allowed) => allowed,
        Err(denied) => {
            deny_connection(&mut client, &sink, denied, "403 Forbidden").await;
            return Ok(());
        }
    };
    sink.record(allowed.event.clone());

    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .context("acknowledge connect")?;

    // The tunnel is only approved once the handshake names the host the CONNECT
    // asked for, so the ClientHello is read BEFORE anything reaches upstream.
    let hello = match read_client_hello(&mut client, &gateway, &allowed.route).await {
        Ok(hello) => hello,
        Err(denied) => {
            sink.record(denied.event.clone());
            return Ok(());
        }
    };

    let mut upstream = match dial(&allowed.route).await {
        Ok(upstream) => upstream,
        Err(error) => {
            debug!(
                "egress gateway cannot reach {}: {error:#}",
                allowed.route.host
            );
            return Ok(());
        }
    };
    upstream
        .write_all(&hello)
        .await
        .context("forward client hello")?;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

async fn handle_plain(
    mut client: TcpStream,
    head: RequestHead,
    gateway: Arc<EgressGateway>,
    sink: Arc<dyn EgressEventSink>,
) -> Result<()> {
    let kind = || RequestKind::Http {
        method: head.method.clone(),
    };
    // A proxy request carries an absolute URL. Origin form means the client is
    // talking to us as if we were the origin server, and `https://` absolute
    // form would need us to terminate TLS — neither is a route.
    let url = match Url::parse(&head.target) {
        Ok(url) if url.scheme() == "http" => url,
        Ok(url) => {
            let request = EgressRequest {
                ctx: EgressContext::default(),
                kind: kind(),
                scheme: EgressScheme::Http,
                host: url.host_str().unwrap_or_default().to_string(),
                port: url.port().unwrap_or(0),
            };
            let denied = gateway.refuse(
                &request,
                DenialReason::ProtocolNotRoutable,
                format!("{} is not routable through the gateway", url.scheme()),
            );
            deny_connection(&mut client, &sink, denied, "403 Forbidden").await;
            return Ok(());
        }
        Err(_) => {
            let request = EgressRequest {
                ctx: EgressContext::default(),
                kind: kind(),
                scheme: EgressScheme::Http,
                host: head.target.clone(),
                port: 0,
            };
            let denied = gateway.refuse(
                &request,
                DenialReason::MalformedRequest,
                "a proxy request must use the absolute form",
            );
            deny_connection(&mut client, &sink, denied, "400 Bad Request").await;
            return Ok(());
        }
    };

    let host = match url.host_str() {
        Some(host) => host.to_string(),
        None => {
            let request = EgressRequest {
                ctx: EgressContext::default(),
                kind: kind(),
                scheme: EgressScheme::Http,
                host: head.target.clone(),
                port: 0,
            };
            let denied =
                gateway.refuse(&request, DenialReason::MalformedRequest, "url has no host");
            deny_connection(&mut client, &sink, denied, "400 Bad Request").await;
            return Ok(());
        }
    };

    let request = EgressRequest {
        ctx: EgressContext::default(),
        kind: kind(),
        scheme: EgressScheme::Http,
        host,
        port: url.port().unwrap_or(80),
    };
    let allowed = match screen(&gateway, request).await {
        Ok(allowed) => allowed,
        Err(denied) => {
            deny_connection(&mut client, &sink, denied, "403 Forbidden").await;
            return Ok(());
        }
    };
    sink.record(allowed.event.clone());

    let mut upstream = match dial(&allowed.route).await {
        Ok(upstream) => upstream,
        Err(error) => {
            debug!(
                "egress gateway cannot reach {}: {error:#}",
                allowed.route.host
            );
            let _ = client
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return Ok(());
        }
    };

    let forwarded = rewrite_request(&head, &url, &allowed.route);
    upstream
        .write_all(&forwarded)
        .await
        .context("forward request head")?;
    if !head.rest.is_empty() {
        upstream
            .write_all(&head.rest)
            .await
            .context("forward request body")?;
    }
    // The response is relayed verbatim, including any 3xx: this proxy does not
    // follow redirects, and the client's next request is screened again.
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

/// Screening resolves DNS, which blocks; keeping it off the reactor is why this
/// hop exists.
async fn screen(gateway: &Arc<EgressGateway>, request: EgressRequest) -> Result<Allowed, Denied> {
    let worker = gateway.clone();
    let screened = request.clone();
    match tokio::task::spawn_blocking(move || worker.screen(&screened)).await {
        Ok(result) => result,
        // A screening that did not finish decided nothing, and nothing decided
        // is nothing allowed.
        Err(error) => Err(gateway.refuse(
            &request,
            DenialReason::Unresolvable,
            format!("screening did not complete: {error}"),
        )),
    }
}

/// A tunnel whose handshake cannot be checked against the `CONNECT` target.
fn sni_denial(gateway: &EgressGateway, route: &PinnedRoute, detail: &str) -> Denied {
    let request = EgressRequest {
        ctx: route.ctx.clone(),
        kind: RequestKind::Connect,
        scheme: route.scheme,
        host: route.host.clone(),
        port: route.port,
    };
    gateway.refuse(&request, DenialReason::SniMismatch, detail)
}

/// Connects to a PINNED address. The name is never resolved again here — that
/// second lookup is exactly the window a rebinding attack needs.
async fn dial(route: &PinnedRoute) -> Result<TcpStream> {
    let mut last_error = None;
    for addr in &route.addresses {
        match timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = Some(format!("{addr}: {error}")),
            Err(_) => last_error = Some(format!("{addr}: connect timed out")),
        }
    }
    Err(anyhow::anyhow!(
        "no address of {} answered: {}",
        route.host,
        last_error.unwrap_or_else(|| "no address".to_string())
    ))
}

async fn read_head(client: &mut TcpStream) -> Result<RequestHead> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > MAX_HEAD_BYTES {
            anyhow::bail!("request head exceeds {MAX_HEAD_BYTES} bytes");
        }
        let read = client.read(&mut chunk).await.context("read request head")?;
        if read == 0 {
            anyhow::bail!("connection closed before a complete request head");
        }
        buf.extend_from_slice(&chunk[..read]);
    };

    let head_text = std::str::from_utf8(&buf[..head_end])
        .context("request head is not valid text")?
        .to_string();
    let rest = buf[head_end + 4..].to_vec();

    let mut lines = head_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .filter(|m| m.bytes().all(|b| b.is_ascii_alphabetic()))
        .context("request line has no method")?
        .to_string();
    let target = parts
        .next()
        .context("request line has no target")?
        .to_string();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').context("malformed header line")?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }

    Ok(RequestHead {
        method,
        target,
        headers,
        rest,
    })
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Accepts both forms real clients send: `Basic` from a proxy URL that carries
/// the token as the password, and `Bearer` from our own components.
fn credential_matches(gateway: &EgressGateway, head: &RequestHead) -> bool {
    let value = match head.header("proxy-authorization") {
        Some(value) => value,
        None => return false,
    };
    let (scheme, credential) = match value.split_once(' ') {
        Some(pair) => pair,
        None => return false,
    };
    if scheme.eq_ignore_ascii_case("bearer") {
        return gateway.token_matches(credential.trim());
    }
    if !scheme.eq_ignore_ascii_case("basic") {
        return false;
    }
    let decoded = match base64::engine::general_purpose::STANDARD.decode(credential.trim()) {
        Ok(decoded) => decoded,
        Err(_) => return false,
    };
    let decoded = match String::from_utf8(decoded) {
        Ok(decoded) => decoded,
        Err(_) => return false,
    };
    match decoded.split_once(':') {
        Some((_user, password)) => gateway.token_matches(password),
        None => gateway.token_matches(&decoded),
    }
}

/// Reads until the ClientHello can be verified. A tunnel that never presents a
/// verifiable name is closed, never forwarded.
async fn read_client_hello(
    client: &mut TcpStream,
    gateway: &EgressGateway,
    route: &PinnedRoute,
) -> Result<Vec<u8>, Denied> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        match scan_client_hello(&buf) {
            SniScan::Found(name) => {
                gateway.verify_sni(route, Some(&name))?;
                return Ok(buf);
            }
            SniScan::Absent => {
                gateway.verify_sni(route, None)?;
                return Ok(buf);
            }
            SniScan::Malformed => {
                return Err(sni_denial(
                    gateway,
                    route,
                    "the tunnel did not begin with a TLS handshake",
                ))
            }
            SniScan::NeedMore => {}
        }
        if buf.len() > MAX_CLIENT_HELLO_BYTES {
            return Err(sni_denial(
                gateway,
                route,
                "the client hello exceeded the read bound before it could be verified",
            ));
        }
        let read = match timeout(HANDSHAKE_TIMEOUT, client.read(&mut chunk)).await {
            Ok(Ok(read)) => read,
            _ => {
                return Err(sni_denial(
                    gateway,
                    route,
                    "the handshake stalled before a server name arrived",
                ))
            }
        };
        if read == 0 {
            return Err(sni_denial(
                gateway,
                route,
                "the client closed the tunnel before the handshake",
            ));
        }
        buf.extend_from_slice(&chunk[..read]);
    }
}

/// Rebuilds the request in origin form for the upstream server, without the
/// hop-by-hop headers and without the proxy credential, and pinned to one
/// request per connection so no second destination can reuse the socket.
fn rewrite_request(head: &RequestHead, url: &Url, route: &PinnedRoute) -> Vec<u8> {
    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    if path.is_empty() {
        path.push('/');
    }

    let mut out = format!("{} {} HTTP/1.1\r\n", head.method, path);
    let mut host_sent = false;
    for (name, value) in &head.headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "proxy-authorization"
                | "proxy-connection"
                | "connection"
                | "keep-alive"
                | "upgrade"
                | "te"
                | "trailer"
        ) {
            continue;
        }
        if lower == "host" {
            host_sent = true;
        }
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    if !host_sent {
        if route.port == 80 {
            out.push_str(&format!("Host: {}\r\n", route.host));
        } else {
            out.push_str(&format!("Host: {}:{}\r\n", route.host, route.port));
        }
    }
    out.push_str("Connection: close\r\n\r\n");
    out.into_bytes()
}

fn split_authority(target: &str, default_port: u16) -> Option<(String, u16)> {
    let target = target.trim();
    if let Some(rest) = target.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None => default_port,
        };
        return Some((host.to_string(), port));
    }
    match target.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !host.contains(':') => {
            Some((host.to_string(), port.parse().ok()?))
        }
        Some(_) => None,
        None => Some((target.to_string(), default_port)),
    }
}

async fn deny_connection(
    client: &mut TcpStream,
    sink: &Arc<dyn EgressEventSink>,
    denied: Denied,
    status: &str,
) {
    sink.record(denied.event.clone());
    let body = format!("egress denied: {}", denied.reason.slug());
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = client.write_all(response.as_bytes()).await;
}

async fn refuse_unparsed(
    client: &mut TcpStream,
    gateway: &Arc<EgressGateway>,
    sink: &Arc<dyn EgressEventSink>,
    reason: DenialReason,
    detail: String,
    status: &str,
) {
    let request = EgressRequest {
        ctx: EgressContext::default(),
        kind: RequestKind::Http {
            method: "?".to_string(),
        },
        scheme: EgressScheme::Http,
        host: String::new(),
        port: 0,
    };
    let denied = gateway.refuse(&request, reason, detail);
    deny_connection(client, sink, denied, status).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::egress::resolver::Resolver;
    use crate::code_studio::egress::{
        DenialReason as Reason, EgressGatewayConfig, EgressOutcome, HostPattern, WorkspaceEgress,
    };
    use crate::code_studio::models::EgressEnforcement;
    use std::sync::Mutex;

    struct StaticResolver;

    impl Resolver for StaticResolver {
        fn resolve(&self, _host: &str, port: u16) -> anyhow::Result<Vec<SocketAddr>> {
            Ok(vec![SocketAddr::new(
                "93.184.216.34".parse().unwrap(),
                port,
            )])
        }
    }

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<EgressEvent>>);

    impl EgressEventSink for RecordingSink {
        fn record(&self, event: EgressEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    async fn proxy_with_sink() -> (SocketAddr, Arc<RecordingSink>) {
        let gateway = match EgressGateway::for_workspace(
            EgressGatewayConfig {
                workspace_id: "ws-1".into(),
                enforcement: EgressEnforcement::Namespace,
                policy: crate::code_studio::egress::EgressPolicy::Any,
                workspace_allowlist: vec![HostPattern::parse("crates.io").unwrap()],
                org_approved: Vec::new(),
                local_services: Vec::new(),
                proxy_token: "proxy-token".into(),
            },
            Arc::new(StaticResolver),
        ) {
            WorkspaceEgress::Enforced(gateway) => gateway,
            WorkspaceEgress::NoMechanism(_) => panic!("expected a gateway"),
        };
        let sink = Arc::new(RecordingSink::default());
        let proxy = EgressProxy::bind(gateway, sink.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = proxy.local_addr().unwrap();
        tokio::spawn(proxy.run());
        (addr, sink)
    }

    async fn speak(addr: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let mut chunk = [0u8; 512];
        // The proxy answers and closes on every path exercised here.
        loop {
            match timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(read)) => response.extend_from_slice(&chunk[..read]),
                Ok(Err(_)) => break,
            }
            if response.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&response).into_owned()
    }

    #[tokio::test]
    async fn a_tunnel_to_an_unlisted_host_is_refused_at_the_proxy() {
        let (addr, sink) = proxy_with_sink().await;
        let response = speak(
            addr,
            "CONNECT evil.example:443 HTTP/1.1\r\nProxy-Authorization: Bearer proxy-token\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");

        let events = sink.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].outcome, EgressOutcome::Denied);
        assert_eq!(events[0].denial, Some(Reason::NotAllowlisted));
        assert_eq!(events[0].host, "evil.example");
    }

    #[tokio::test]
    async fn a_request_without_the_proxy_credential_never_reaches_screening() {
        let (addr, sink) = proxy_with_sink().await;
        let response = speak(addr, "CONNECT crates.io:443 HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 407"), "{response}");

        let events = sink.0.lock().unwrap();
        assert_eq!(events[0].denial, Some(Reason::ProxyAuth));
    }

    #[tokio::test]
    async fn a_protocol_that_is_not_http_has_no_route_through_the_gateway() {
        let (addr, sink) = proxy_with_sink().await;
        // What an ssh client says first. There is no tunnel to take.
        let response = speak(addr, "SSH-2.0-OpenSSH_9.6\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");

        let events = sink.0.lock().unwrap();
        assert_eq!(events[0].denial, Some(Reason::MalformedRequest));
    }

    #[tokio::test]
    async fn an_absolute_form_request_to_another_scheme_is_refused() {
        let (addr, sink) = proxy_with_sink().await;
        let response = speak(
            addr,
            "GET ftp://crates.io/x HTTP/1.1\r\nProxy-Authorization: Bearer proxy-token\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");

        let events = sink.0.lock().unwrap();
        assert_eq!(events[0].denial, Some(Reason::ProtocolNotRoutable));
    }

    #[test]
    fn a_connect_target_is_split_into_host_and_port() {
        assert_eq!(
            split_authority("crates.io:443", 443),
            Some(("crates.io".to_string(), 443))
        );
        assert_eq!(
            split_authority("[fd00::1]:8443", 443),
            Some(("fd00::1".to_string(), 8443))
        );
        assert_eq!(
            split_authority("crates.io", 443),
            Some(("crates.io".to_string(), 443))
        );
        assert_eq!(split_authority("crates.io:not-a-port", 443), None);
    }

    #[test]
    fn the_header_terminator_is_found_only_when_complete() {
        assert_eq!(find_header_end(b"GET / HTTP/1.1\r\n\r\n"), Some(14));
        assert_eq!(find_header_end(b"GET / HTTP/1.1\r\n"), None);
    }
}
