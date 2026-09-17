// ============ File: linux_sandbox_net.rs — the sandbox's only route out of its network namespace ============
//
// The Linux sandbox keeps its network namespace unshared: it has no route to
// the host, no resolver, and no way to reach another service on this machine.
// The egress proxy still has to answer at exactly the endpoint the CLI is
// configured with — `http://tf:<token>@127.0.0.1:<port>` — because that
// credentialed URL is one contract shared with macOS, not a per-platform
// detail.
//
// So the endpoint is served INSIDE the namespace. A unix socket of the host is
// bind-mounted into the sandbox and this binary is started as bubblewrap's
// initial child: it binds `127.0.0.1:<port>` on the namespace's own loopback
// (bubblewrap brings `lo` up whenever it unshares the network) and relays every
// accepted connection to that socket. The host end of the same socket relays to
// the proxy's loopback listener, so nothing about the proxy's authentication,
// screening or revocation changes: when the proxy task is aborted its listener
// stops answering and the relay has nowhere to forward.

use super::private_root::PrivateRoot;
use anyhow::{Context, Result};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const FORWARDER: &str = "--tentaflow-sandbox-network";
const ACCEPT_POLL: Duration = Duration::from_millis(50);
const SOCKET_NAME: &str = "proxy.sock";
const SOCKET_PREFIX: &str = "tfn-";

/// Concurrent relayed connections, per side. A CLI opens a handful of HTTP
/// connections; a runaway or hostile one would otherwise turn an unbounded
/// accept loop into unbounded threads and descriptors on the host. Reaching the
/// cap closes the newest connection, which the client sees as a refused proxy
/// rather than as a hang.
pub const MAX_RELAYS: usize = 64;

/// Consecutive failing `accept` calls tolerated before an accept loop gives up.
/// One `EMFILE` must not end a loop whose port stays bound, and a listener that
/// is permanently broken must not spin.
const MAX_ACCEPT_FAILURES: u32 = 64;

/// Paced so a client hammering a full transport cannot also spin the accept
/// loop, and so the log says how many were refused instead of one line each.
const REFUSAL_BACKOFF: Duration = Duration::from_millis(20);
const REFUSAL_REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// How long a relayed connection may be idle in BOTH directions.
///
/// Most of what crosses here is a `CONNECT` tunnel the egress proxy relays
/// verbatim, so a connection is NOT one request: an HTTP client keeps its TLS
/// tunnel pooled between calls, and the tunnel outlives any single exchange.
/// That is why the bound is on idleness and not on age — bytes in either
/// direction reset the clock, so a long download, a streamed answer or a busy
/// pooled tunnel is never idle, while a tunnel nobody is using stops costing a
/// relay slot, two threads and a gateway connection. Closing an idle pooled
/// tunnel is not a failure the CLI has to handle specially: its client opens a
/// new one on the next request, which is the same thing that happens whenever a
/// remote server times a pooled connection out. Ten minutes is long enough that
/// only a genuinely unused tunnel reaches it.
const RELAY_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// How often an idle pump wakes to re-check. Short enough that a closed relay
/// is released promptly, long enough not to matter.
const IDLE_POLL: Duration = Duration::from_secs(5);

/// The two bounds a relayed connection lives under. Parameters rather than
/// constants so their regressions are testable without waiting them out.
#[derive(Debug, Clone, Copy)]
pub struct RelayTimeouts {
    /// How long the opening bytes may take to arrive. A connection that says
    /// nothing costs a thread and two descriptors, and the endpoint it reached
    /// is one hop from the egress gateway.
    pub opening: Duration,
    pub idle: Duration,
}

impl Default for RelayTimeouts {
    fn default() -> Self {
        Self {
            opening: Duration::from_secs(30),
            idle: RELAY_IDLE_TIMEOUT,
        }
    }
}

/// Last time either direction of one relayed connection moved a byte.
struct Activity {
    base: Instant,
    millis: AtomicU64,
}

impl Activity {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            millis: AtomicU64::new(0),
        }
    }

    fn touch(&self) {
        self.millis
            .store(self.base.elapsed().as_millis() as u64, Ordering::Release);
    }

    fn idle_for(&self) -> Duration {
        self.base
            .elapsed()
            .saturating_sub(Duration::from_millis(self.millis.load(Ordering::Acquire)))
    }
}

/// Counts live relays so the accept loops can refuse past [`MAX_RELAYS`].
/// Released when the relay's thread ends, whichever way it ended.
struct RelaySlot(Arc<AtomicUsize>);

impl RelaySlot {
    /// `None` when the cap is already reached.
    fn claim(live: &Arc<AtomicUsize>) -> Option<Self> {
        live.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < MAX_RELAYS).then_some(count + 1)
        })
        .ok()
        .map(|_| Self(live.clone()))
    }
}

impl Drop for RelaySlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Runs the forwarder when this process was started as a sandbox's init, and
/// returns the exit code the caller must exit with. Returns `None` for every
/// ordinary start of the binary.
pub fn maybe_run() -> Option<i32> {
    let argv = entry_argv();
    if argv.get(1).map(String::as_str) != Some(FORWARDER) {
        return None;
    }
    Some(match forward(&argv) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("sandbox network: {error:#}");
            125
        }
    })
}

/// Under `cargo test` the sandboxed executable is the test binary, which parses
/// its own argv; the invocation then arrives in the environment instead.
fn entry_argv() -> Vec<String> {
    #[cfg(test)]
    if let Ok(packed) = std::env::var("TENTAFLOW_SANDBOX_NETWORK_TEST_ARGV") {
        return packed.split('\u{1}').map(str::to_string).collect();
    }
    std::env::args().collect()
}

fn forward(argv: &[String]) -> Result<i32> {
    let spec: serde_json::Value = serde_json::from_str(
        argv.get(2)
            .context("missing sandbox network specification")?,
    )?;
    let port = spec["port"]
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .context("invalid sandbox proxy port")?;
    let socket = PathBuf::from(
        spec["socket"]
            .as_str()
            .context("missing sandbox proxy socket")?,
    );
    // Bound BEFORE the CLI is executed. A request that raced the forwarder would
    // find a closed port and be reported as a provider outage.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .context("bind the proxy endpoint inside the sandbox")?;
    let program = argv.get(3).context("missing sandboxed command")?;
    // Proven before the CLI starts, so a socket that did not survive the bind
    // mount is reported here instead of reaching the CLI as a provider outage.
    // Opening and closing costs nothing upstream: the host end dials the
    // gateway only once the sandbox has written something.
    drop(UnixStream::connect(&socket).with_context(|| {
        format!(
            "reach the host proxy socket {} from the sandbox",
            socket.display()
        )
    })?);
    std::thread::spawn(move || accept_inside(listener, socket));
    let status = std::process::Command::new(program)
        .args(&argv[4..])
        .status()
        .with_context(|| format!("start {program} inside the sandbox"))?;
    // This process is bubblewrap's initial child: its exit is what makes
    // bubblewrap's reaper tear the pid namespace down, and with it everything
    // the CLI forked, detached or orphaned.
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(libc::SIGKILL)))
}

fn accept_inside(listener: TcpListener, socket: PathBuf) {
    let live = Arc::new(AtomicUsize::new(0));
    let timeouts = RelayTimeouts::default();
    let mut failures = 0_u32;
    let mut refusals = Refusals::new("sandbox network");
    loop {
        let client = match listener.accept() {
            Ok((client, _)) => {
                failures = 0;
                client
            }
            // A descriptor or memory limit hit while accepting is transient: the
            // port stays bound and the next request must still be served, so
            // this loop cannot end on the first one.
            Err(error) => {
                failures += 1;
                if failures > MAX_ACCEPT_FAILURES {
                    eprintln!("sandbox network: giving up on the proxy endpoint: {error}");
                    return;
                }
                eprintln!("sandbox network: accept failed, retrying: {error}");
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        };
        let Some(slot) = RelaySlot::claim(&live) else {
            refusals.record();
            continue;
        };
        let socket = socket.clone();
        std::thread::spawn(move || {
            let _slot = slot;
            match UnixStream::connect(&socket) {
                Ok(upstream) => couple(client, upstream, timeouts.idle),
                Err(error) => eprintln!("sandbox network: the host proxy socket refused: {error}"),
            }
        });
    }
}

/// Refusals are the one event a client controls the rate of, so they are
/// counted and reported periodically rather than printed one line each, and
/// refusing costs the caller a short wait so a full transport cannot be turned
/// into a spin.
struct Refusals {
    who: &'static str,
    last_report: Instant,
    since_report: u64,
}

impl Refusals {
    fn new(who: &'static str) -> Self {
        Self {
            who,
            // Reports the first refusal immediately; coalesces the rest.
            last_report: Instant::now() - REFUSAL_REPORT_INTERVAL,
            since_report: 0,
        }
    }

    fn record(&mut self) {
        self.since_report += 1;
        if self.last_report.elapsed() >= REFUSAL_REPORT_INTERVAL {
            eprintln!(
                "{}: refused {} connection(s) past {MAX_RELAYS} in flight",
                self.who, self.since_report
            );
            self.last_report = Instant::now();
            self.since_report = 0;
        }
        std::thread::sleep(REFUSAL_BACKOFF);
    }
}

/// The host end of the transport: one unix socket per proxy, relayed to that
/// proxy's loopback listener. It is owned by whoever owns the proxy, so the
/// socket disappears together with the gateway it leads to.
pub struct HostRelay {
    /// Removes the directory when dropped, and holds the lock that tells a
    /// concurrent relay's sweep this one is alive.
    _root: PrivateRoot,
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    /// Kept only so the regression tests for [`MAX_RELAYS`] can observe the
    /// bound they assert; the accept loop owns the counter it enforces.
    #[cfg(test)]
    live: Arc<AtomicUsize>,
}

impl HostRelay {
    pub fn open(address: SocketAddr) -> Result<Self> {
        Self::open_with(address, RelayTimeouts::default())
    }

    /// The bounds are parameters so a test can watch a silent or idle
    /// connection be released without waiting out the production timeouts.
    pub fn open_with(address: SocketAddr, timeouts: RelayTimeouts) -> Result<Self> {
        PrivateRoot::sweep(SOCKET_PREFIX);
        let root = PrivateRoot::claim(SOCKET_PREFIX)?;
        let socket = root.path().join(SOCKET_NAME);
        let listener = UnixListener::bind(&socket)
            .with_context(|| format!("bind the sandbox transport at {}", socket.display()))?;
        // Polling instead of a blocking accept: dropping this relay has to stop
        // the thread, and an accept that nobody can interrupt would outlive the
        // gateway it forwards to.
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicUsize::new(0));
        let worker = Relaying {
            listener,
            address,
            stop: stop.clone(),
            live: live.clone(),
            timeouts,
        };
        std::thread::spawn(move || accept_on_host(worker));
        Ok(Self {
            _root: root,
            socket,
            stop,
            #[cfg(test)]
            live,
        })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Connections this relay is currently carrying. Bounded by
    /// [`MAX_RELAYS`]; read by the regression tests for that bound.
    #[cfg(test)]
    pub fn live_relays(&self) -> usize {
        self.live.load(Ordering::Acquire)
    }
}

/// What one host-side accept loop needs. A struct because the alternative is
/// five positional arguments of which two are counters.
struct Relaying {
    listener: UnixListener,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    live: Arc<AtomicUsize>,
    timeouts: RelayTimeouts,
}

impl Drop for HostRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn accept_on_host(relaying: Relaying) {
    let Relaying {
        listener,
        address,
        stop,
        live,
        timeouts,
    } = relaying;
    let mut failures = 0_u32;
    let mut refusals = Refusals::new("sandbox transport");
    while !stop.load(Ordering::Acquire) {
        let client = match listener.accept() {
            Ok((client, _)) => {
                failures = 0;
                client
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
            // Same reason as inside the sandbox: a transient descriptor limit
            // must not retire a socket the CLI is configured to use.
            Err(error) => {
                failures += 1;
                if failures > MAX_ACCEPT_FAILURES {
                    eprintln!("sandbox transport: giving up on the host socket: {error}");
                    return;
                }
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        };
        let Some(slot) = RelaySlot::claim(&live) else {
            refusals.record();
            continue;
        };
        std::thread::spawn(move || {
            let _slot = slot;
            if client.set_nonblocking(false).is_ok() {
                serve_on_host(client, address, timeouts);
            }
        });
    }
}

fn serve_on_host(mut client: UnixStream, address: SocketAddr, timeouts: RelayTimeouts) {
    // The gateway is dialled only once the sandbox has something to say, so the
    // readiness probe's connect is not screened — and logged — as a request
    // that never arrived. Bounded, because until those bytes arrive this is a
    // thread and two descriptors spent on a connection that may never speak.
    if client.set_read_timeout(Some(timeouts.opening)).is_err() {
        return;
    }
    let mut opening = [0u8; 4096];
    let read = match client.read(&mut opening) {
        Ok(0) | Err(_) => return,
        Ok(read) => read,
    };
    let Ok(mut upstream) = TcpStream::connect(address) else {
        return;
    };
    if upstream.write_all(&opening[..read]).is_err() {
        return;
    }
    // From here the idle bound takes over from the opening one: the pumps set
    // their own read deadlines, so one byte followed by silence no longer buys
    // a permanent slot.
    couple(client, upstream, timeouts.idle);
}

/// The arguments that replace the CLI as bubblewrap's initial child. The
/// forwarder is THIS executable: a second helper binary would be another
/// artifact to install, sign and keep in step with the policy it serves.
pub fn entry_command(port: u16, socket: &Path) -> Result<Vec<String>> {
    Ok(vec![
        host_executable()?.display().to_string(),
        FORWARDER.into(),
        json!({"port": port, "socket": socket}).to_string(),
    ])
}

pub fn host_executable() -> Result<PathBuf> {
    std::env::current_exe()
        .context("sandbox forwarder executable")?
        .canonicalize()
        .context("resolve the sandbox forwarder executable")
}

pub trait RelayStream: Read + Write + Send + Sized + 'static {
    fn duplicate(&self) -> std::io::Result<Self>;
    fn finish(&self);
    fn close(&self);
    /// Both directions are bounded: blocking in a write to a peer that stopped
    /// reading pins exactly as much as blocking in a read from one that stopped
    /// writing, and the idle clock is consulted in neither while blocked.
    fn deadlines(&self, every: Duration) -> std::io::Result<()>;
}

impl RelayStream for TcpStream {
    fn duplicate(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
    fn finish(&self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
    fn close(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
    fn deadlines(&self, every: Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(every))?;
        self.set_write_timeout(Some(every))
    }
}

impl RelayStream for UnixStream {
    fn duplicate(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
    fn finish(&self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
    fn close(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
    fn deadlines(&self, every: Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(every))?;
        self.set_write_timeout(Some(every))
    }
}

/// Two blocking pumps, one per direction, each ending in a half close so the
/// peer reads an end of stream instead of waiting out its own timeout. Both
/// share one activity clock, so `idle` is reached only when NEITHER direction
/// has moved a byte for that long.
pub fn couple<A: RelayStream, B: RelayStream>(client: A, upstream: B, idle: Duration) {
    let (Ok(client_out), Ok(upstream_out)) = (client.duplicate(), upstream.duplicate()) else {
        return;
    };
    let (Ok(client_stop), Ok(upstream_stop)) = (client.duplicate(), upstream.duplicate()) else {
        return;
    };
    let activity = Arc::new(Activity::new());
    let outbound = {
        let activity = activity.clone();
        std::thread::spawn(move || pump(client, upstream_out, &activity, idle))
    };
    pump(upstream, client_out, &activity, idle);
    // The inbound direction has ended, which means upstream is gone: either the
    // gateway closed its answer or the CONNECT tunnel behind it collapsed.
    // Whichever it was, there is nowhere left to carry the client's remaining
    // bytes, so holding its write half open would pin this join, its thread and
    // the relay slot with no destination. Closing both halves is what lets the
    // outbound pump return.
    client_stop.close();
    upstream_stop.close();
    let _ = outbound.join();
}

/// One direction. Neither the read nor the write blocks indefinitely, so a
/// connection that has gone silent in both directions can be let go without
/// cutting off one that is merely slow.
fn pump<R: RelayStream, W: RelayStream>(
    mut from: R,
    mut to: W,
    activity: &Activity,
    idle: Duration,
) {
    let slice = IDLE_POLL.min(idle);
    if from.deadlines(slice).is_err() || to.deadlines(slice).is_err() {
        to.finish();
        return;
    }
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                activity.touch();
                if !hand_on(&mut to, &buffer[..read], activity, idle) {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if would_block(&error) => {
                if activity.idle_for() >= idle {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    to.finish();
}

/// `write_all` with the same idle bound as the read side. Written by hand
/// because `write_all` cannot report how much of the buffer a timed-out write
/// had already delivered, and a peer that stops reading must not be able to
/// park this thread in a single call.
fn hand_on<W: RelayStream>(
    to: &mut W,
    mut pending: &[u8],
    activity: &Activity,
    idle: Duration,
) -> bool {
    while !pending.is_empty() {
        match to.write(pending) {
            Ok(0) => return false,
            Ok(written) => {
                pending = &pending[written..];
                // Progress, even partial, is not silence: a slow but moving
                // consumer keeps its connection.
                activity.touch();
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if would_block(&error) => {
                if activity.idle_for() >= idle {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

fn would_block(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}
