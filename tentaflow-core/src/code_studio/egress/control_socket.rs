// ===== File: code_studio/egress/control_socket.rs — local listeners with a peer check =====
//
// The git broker, the provider adapter and the agent bridge listen HERE, never
// on `127.0.0.1`. In native mode the loopback interface is open to every
// process on the host, so a TCP listener would accept the workspace's own
// sandbox, another user's shell and anything else that guessed the port (§7.6).
// A unix socket (Windows: a named pipe) at least names its peer.
//
// The honest limitation, stated in the plan and repeated here so nobody reads
// more into this than it gives: when the agent process runs as the SAME user as
// Core — which is exactly what `trusted_native` means — the peer check cannot
// tell them apart. It rejects a different user, not a different program of the
// same user. What separates the agent's calls from Core's own is then the shim
// token alone, bound to the run and the capability (§7.3), plus the operation
// log. The peer check is the floor, not the guarantee.
//
// Peer identification fails CLOSED: a connection whose credentials cannot be
// read is refused, never accepted as anonymous.

use std::path::Path;

use anyhow::{Context, Result};

/// Who is on the other end. Fields are `Option` because platforms differ in
/// what they can prove, and a value we cannot obtain must not be invented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    pub pid: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl PeerIdentity {
    /// `Some(true)` when the peer runs as the account Core runs as, `Some(false)`
    /// when it demonstrably does not, `None` when this platform cannot say.
    /// A `Some(true)` is NOT an authorization — see the module header.
    pub fn same_account_as_core(&self) -> Option<bool> {
        #[cfg(unix)]
        {
            self.uid.map(|uid| uid == unsafe { libc::geteuid() })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// Binds the control socket, replacing a stale one left by a previous run.
///
/// Unix: a socket file at `path`, mode `0600` inside a `0700` directory — the
/// filesystem is the first filter, the peer check the second.
///
/// Windows: a named pipe called `\\.\pipe\<file name of path>`; the directory
/// part is ignored because pipes do not live in the filesystem namespace.
///
/// Must be called from a Tokio runtime: the listener registers with the
/// reactor.
pub fn bind_control_socket(path: &Path) -> Result<ControlListener> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create control socket dir {}", parent.display()))?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("restrict control socket dir {}", parent.display()))?;
        }
        if path.exists() {
            std::fs::remove_file(path)
                .with_context(|| format!("remove stale control socket {}", path.display()))?;
        }
        let listener = tokio::net::UnixListener::bind(path)
            .with_context(|| format!("bind control socket {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restrict control socket {}", path.display()))?;
        Ok(ControlListener { listener })
    }
    #[cfg(windows)]
    {
        let name = pipe_name(path)?;
        let server = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .with_context(|| format!("create control pipe {name}"))?;
        Ok(ControlListener {
            name,
            server: Some(server),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        anyhow::bail!(
            "control sockets need a unix socket or a named pipe; {} has neither",
            path.display()
        )
    }
}

#[cfg(windows)]
fn pipe_name(path: &Path) -> Result<String> {
    let raw = path.to_string_lossy();
    if raw.starts_with(r"\\.\pipe\") {
        return Ok(raw.into_owned());
    }
    let file = path
        .file_name()
        .context("control socket path has no final component")?
        .to_string_lossy()
        .into_owned();
    Ok(format!(r"\\.\pipe\{file}"))
}

#[cfg(unix)]
pub struct ControlListener {
    listener: tokio::net::UnixListener,
}

#[cfg(unix)]
pub type ControlStream = tokio::net::UnixStream;

#[cfg(unix)]
impl ControlListener {
    /// Accepts one connection and identifies its peer. An unidentifiable peer
    /// is an error, not an anonymous client.
    pub async fn accept(&mut self) -> Result<(ControlStream, PeerIdentity)> {
        let (stream, _addr) = self
            .listener
            .accept()
            .await
            .context("accept control socket")?;
        let cred = stream
            .peer_cred()
            .context("read peer credentials of a control socket client")?;
        let identity = PeerIdentity {
            pid: cred.pid().map(|pid| pid as u32),
            uid: Some(cred.uid()),
            gid: Some(cred.gid()),
        };
        Ok((stream, identity))
    }
}

#[cfg(windows)]
pub struct ControlListener {
    name: String,
    server: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
}

#[cfg(windows)]
pub type ControlStream = tokio::net::windows::named_pipe::NamedPipeServer;

#[cfg(windows)]
// `GetNamedPipeClientProcessId` lives in kernel32, which every Windows target
// already links. It is declared here rather than pulled from `windows-sys`
// because the pipe API sits behind a crate feature this build does not enable.
extern "system" {
    fn GetNamedPipeClientProcessId(pipe: *mut std::ffi::c_void, client_process_id: *mut u32)
        -> i32;
}

#[cfg(windows)]
impl ControlListener {
    pub async fn accept(&mut self) -> Result<(ControlStream, PeerIdentity)> {
        use std::os::windows::io::AsRawHandle;

        let server = self
            .server
            .take()
            .context("control pipe listener has no free instance")?;
        server.connect().await.context("accept control pipe")?;

        // The next instance has to exist before this one is handed out, or the
        // pipe name would briefly not exist and a client could squat on it.
        self.server = Some(
            tokio::net::windows::named_pipe::ServerOptions::new()
                .create(&self.name)
                .with_context(|| format!("create next instance of {}", self.name))?,
        );

        let mut pid: u32 = 0;
        let ok = unsafe { GetNamedPipeClientProcessId(server.as_raw_handle() as _, &mut pid) };
        if ok == 0 {
            anyhow::bail!("cannot identify the client process of {}", self.name);
        }
        Ok((
            server,
            PeerIdentity {
                pid: Some(pid),
                uid: None,
                gid: None,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn a_control_socket_names_its_peer_and_is_not_reachable_over_tcp() {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("control.sock");
        let mut listener = bind_control_socket(&path).unwrap();

        // The socket file, not a port: nothing here is bound to 127.0.0.1.
        assert!(path.exists());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the socket must not be group- or world-usable");

        let connect_path = path.clone();
        let client = tokio::spawn(async move {
            let mut stream = tokio::net::UnixStream::connect(&connect_path)
                .await
                .unwrap();
            stream.write_all(b"hello").await.unwrap();
        });

        let (_stream, peer) = listener.accept().await.unwrap();
        client.await.unwrap();

        assert_eq!(peer.uid, Some(unsafe { libc::geteuid() }));
        assert_eq!(peer.same_account_as_core(), Some(true));
        // The caveat in the module header, as a test: the check confirms the
        // account, and the test process IS that account, so it cannot tell the
        // two programs apart.
        assert!(peer.pid.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_socket_file_does_not_block_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        std::fs::write(&path, b"stale").unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        assert!(bind_control_socket(&path).is_ok());
    }
}
