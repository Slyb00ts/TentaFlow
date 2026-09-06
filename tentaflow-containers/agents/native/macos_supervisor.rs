// ============ File: macos_supervisor.rs — launchd coalition ownership for sandbox descendants ============

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const FRONTEND: &str = "--tentaflow-process-supervisor";
const WORKER: &str = "--tentaflow-process-worker";
const MAX_MESSAGE: usize = 1024 * 1024;
static HOST_INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn maybe_run() -> Option<i32> {
    HOST_INITIALIZED.store(true, std::sync::atomic::Ordering::Release);
    #[cfg(test)]
    let test_mode = std::env::var("TENTAFLOW_SUPERVISOR_TEST_MODE").ok();
    #[cfg(not(test))]
    let test_mode: Option<String> = None;
    let mode = test_mode.or_else(|| std::env::args().nth(1))?;
    if mode != FRONTEND && mode != WORKER {
        return None;
    }
    let result = (|| {
        #[cfg(test)]
        let test_argument = std::env::var("TENTAFLOW_SUPERVISOR_TEST_ARGUMENT").ok();
        #[cfg(not(test))]
        let test_argument: Option<String> = None;
        let argument = test_argument
            .or_else(|| std::env::args().nth(2))
            .context("missing supervisor argument")?;
        if mode == FRONTEND {
            frontend(&serde_json::from_str(&argument)?)
        } else {
            worker(Path::new(&argument))
        }
    })();
    Some(match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("process supervisor: {error:#}");
            125
        }
    })
}

pub fn new_root() -> Result<PathBuf> {
    Ok(Path::new("/private/tmp").join(format!("tfp-{}", nonce()?)))
}

pub fn wrap(argv: Vec<String>, cwd: &Path, root: &Path) -> Result<Vec<String>> {
    let executable = host_executable()?;
    directory(root)?;
    let invocation = root.join(nonce()?[..8].to_string());
    directory(&invocation)?;
    Ok(vec![
        executable.display().to_string(),
        FRONTEND.into(),
        json!({
            "argv": argv, "cwd": cwd, "root": root, "invocation": invocation,
        })
        .to_string(),
    ])
}

fn host_executable() -> Result<PathBuf> {
    #[cfg(test)]
    {
        if let Some(path) = std::env::var_os("TENTAFLOW_SUPERVISOR_TEST_HOST") {
            return PathBuf::from(path)
                .canonicalize()
                .context("test supervisor host");
        }
        static HELPER: std::sync::OnceLock<std::result::Result<PathBuf, String>> =
            std::sync::OnceLock::new();
        return HELPER
            .get_or_init(|| test_executable().map_err(|error| error.to_string()))
            .clone()
            .map_err(anyhow::Error::msg);
    }
    #[cfg(not(test))]
    {
        std::env::current_exe().context("supervisor executable")
    }
}

#[cfg(test)]
fn test_executable() -> Result<PathBuf> {
    let root = Path::new("/private/tmp").join(format!("tf-supervisor-test-{}", nonce()?));
    directory(&root)?;
    let path = root.join("host");
    let entry = format!(
        "{}::tests::supervisor_test_entrypoint",
        module_path!()
            .split_once("::")
            .unwrap()
            .1
            .rsplit_once("::")
            .unwrap()
            .0
    );
    let library_environment = ["DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| format!(".env({name:?}, {value:?})"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"
use std::os::unix::process::CommandExt;
unsafe extern "C" {{ fn dup2(old: i32, new: i32) -> i32; }}
fn main() {{
    let args: Vec<String> = std::env::args().collect();
    unsafe {{ for fd in 0..3 {{ assert!(dup2(fd, fd + 3) >= 0); }} }}
    let error = std::process::Command::new({executable:?})
        .args(["--exact", {entry:?}, "--nocapture"])
        .env("TENTAFLOW_SUPERVISOR_TEST_MODE", &args[1])
        .env("TENTAFLOW_SUPERVISOR_TEST_ARGUMENT", &args[2])
        .env("TENTAFLOW_SUPERVISOR_TEST_HOST", std::env::current_exe().unwrap())
        {library_environment}
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).exec();
    panic!("{{error}}");
}}
"#,
        executable = std::env::current_exe()?.to_string_lossy(),
        entry = entry,
        library_environment = library_environment,
    );
    let source_path = root.join("host.rs");
    write_private(&source_path, source.as_bytes())?;
    let result = Command::new("rustc")
        .arg("--edition=2021")
        .arg(&source_path)
        .arg("-o")
        .arg(&path)
        .output()?;
    if !result.status.success() {
        bail!(
            "could not build supervisor test host: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

pub fn check_available() -> Result<()> {
    if !cfg!(test) && !HOST_INITIALIZED.load(std::sync::atomic::Ordering::Acquire) {
        bail!("this executable has not initialized the process supervisor entry point");
    }
    if coalition(std::process::id() as i32)? == 0 {
        bail!("missing resource coalition");
    }
    let status = Command::new("/bin/launchctl")
        .args(["print", &domain()])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("process isolation requires the current user's GUI launchd domain");
    }
    Ok(())
}

pub fn ensure_quiescent(root: &Path) -> Result<()> {
    match std::fs::read_dir(root) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                bail!("sandbox descendants have not completed verified cleanup");
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn nonce() -> Result<String> {
    let mut bytes = [0u8; 12];
    if unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::geteuid() })
}

fn directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    use std::os::unix::fs::MetadataExt;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!("invalid private supervisor directory");
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("invalid supervisor field {key}"))
}

fn frontend(request: &Value) -> Result<i32> {
    check_available()?;
    let root = PathBuf::from(field(request, "root")?);
    if root.parent() != Some(Path::new("/private/tmp"))
        || !root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .starts_with("tfp-")
    {
        bail!("invalid supervisor state root");
    }
    directory(&root)?;
    let invocation = PathBuf::from(field(request, "invocation")?);
    if invocation.parent() != Some(root.as_path()) || !invocation.is_dir() {
        bail!("missing process launch intent");
    }
    let socket = invocation.join("s");
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    let label = format!("com.tentaflow.process.{}", nonce()?);
    let token = nonce()?;
    let executable = host_executable()?;
    let spec_path = invocation.join("spec.json");
    let environment = permitted_environment();
    let spec = json!({"argv":request["argv"], "cwd":request["cwd"], "token": token,
        "socket":socket, "label":label, "parent_coalition":coalition(std::process::id() as i32)?,
        "environment":environment, "invocation":invocation});
    write_private(&spec_path, &serde_json::to_vec(&spec)?)?;
    let plist = invocation.join("job.plist");
    write_private(
        &plist,
        job_plist(&label, &executable, &spec_path)?.as_bytes(),
    )?;
    let bootstrap = Command::new("/bin/launchctl")
        .args(["bootstrap", &domain()])
        .arg(&plist)
        .env_clear()
        .stdin(Stdio::null())
        .output()?;
    if !bootstrap.status.success() {
        std::fs::remove_dir_all(&invocation)?;
        bail!(
            "launchd refused the process supervisor: {}",
            String::from_utf8_lossy(&bootstrap.stderr)
        );
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => {
                bootout(&label);
                bail!("supervisor handoff failed; state retained: {error}");
            }
        }
    };
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    let hello = receive_json(&mut stream)?;
    let pid = hello["pid"].as_i64().context("worker pid")? as i32;
    let resource = hello["coalition"].as_u64().context("worker coalition")?;
    if hello["token"] != token
        || resource == 0
        || resource == coalition(std::process::id() as i32)?
        || coalition(pid)? != resource
    {
        bootout(&label);
        bail!("supervisor ownership handshake failed; state retained");
    }
    if unsafe { libc::isatty(0) } == 1 {
        let terminal = unsafe { libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if terminal >= 0 {
            unsafe {
                libc::close(terminal);
            }
            bail!("supervisor frontend must receive an unclaimed PTY");
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENXIO) {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    send_descriptors(&stream)?;
    stream.set_read_timeout(None)?;
    let result = receive_json(&mut stream);
    bootout(&label);
    let result = result?;
    if !result["clean"].as_bool().unwrap_or(false) {
        bail!("descendant cleanup could not be verified; state retained");
    }
    ensure_quiescent(&invocation)?;
    Ok(result["code"].as_i64().context("worker exit status")? as i32)
}

struct WorkerCleanup {
    resource: u64,
    invocation: PathBuf,
    label: String,
    armed: bool,
}

impl Drop for WorkerCleanup {
    fn drop(&mut self) {
        if self.armed && terminate_coalition(self.resource).is_ok() {
            let _ = std::fs::remove_dir_all(&self.invocation);
            bootout(&self.label);
        }
    }
}

fn worker(spec_path: &Path) -> Result<i32> {
    let metadata = std::fs::metadata(spec_path)?;
    if metadata.len() > MAX_MESSAGE as u64 {
        bail!("oversized worker specification");
    }
    let spec: Value = serde_json::from_slice(&std::fs::read(spec_path)?)?;
    let resource = coalition(std::process::id() as i32)?;
    if Some(resource) == spec["parent_coalition"].as_u64() || resource == 0 {
        bail!("launchd did not assign a private resource coalition");
    }
    let initial = members(resource)?;
    if initial
        .iter()
        .any(|member| member.pid != std::process::id() as i32)
    {
        bail!("launchd coalition already contains unrelated processes");
    }
    let mut cleanup = WorkerCleanup {
        resource,
        invocation: PathBuf::from(field(&spec, "invocation")?),
        label: field(&spec, "label")?.into(),
        armed: true,
    };
    let mut stream = UnixStream::connect(field(&spec, "socket")?)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    send_json(
        &mut stream,
        &json!({"token":spec["token"],"pid":std::process::id(),"coalition":resource}),
    )?;
    let descriptors = receive_descriptors(&stream)?;
    stream.set_read_timeout(None)?;
    stream.set_nonblocking(true)?;
    let argv: Vec<String> = serde_json::from_value(spec["argv"].clone())?;
    let environment: Vec<(String, String)> = serde_json::from_value(spec["environment"].clone())?;
    if argv.is_empty() || argv[0] != "/usr/bin/sandbox-exec" {
        bail!("worker requires the native sandbox wrapper");
    }
    let [stdin, stdout, stderr] = descriptors;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(environment)
        .current_dir(field(&spec, "cwd")?)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::isatty(0) == 1 && libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let mut connected = true;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
        }
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(0) | Ok(_) => {
                connected = false;
                break 130;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => {
                connected = false;
                break 130;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let clean = terminate_coalition(resource).is_ok();
    if clean {
        let _ = child.wait();
    }
    if clean {
        std::fs::remove_dir_all(field(&spec, "invocation")?)?;
    }
    cleanup.armed = !clean;
    if connected {
        stream.set_nonblocking(false)?;
        let _ = send_json(&mut stream, &json!({"code":status,"clean":clean}));
    } else {
        bootout(field(&spec, "label")?);
    }
    if !clean {
        bail!("coalition cleanup failed; admission remains locked");
    }
    Ok(status)
}

#[derive(Clone, Copy)]
struct Member {
    pid: i32,
    started: (u64, u64),
}

fn coalition(pid: i32) -> Result<u64> {
    // This flavor is SPI. Validate its complete response size on every call;
    // unsupported OS versions must never degrade to process-group cleanup.
    let mut info = [0u64; 5];
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            20,
            0,
            info.as_mut_ptr().cast(),
            std::mem::size_of_val(&info) as i32,
        )
    };
    if bytes != std::mem::size_of_val(&info) as i32 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(info[0])
}

fn members(resource: u64) -> Result<Vec<Member>> {
    let mut capacity = 4096;
    let pids = loop {
        let mut pids = vec![0i32; capacity];
        let count =
            unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
        if count < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if count as usize >= pids.len() {
            capacity *= 2;
            if capacity > 1_048_576 {
                bail!("process enumeration exceeds safety bound");
            }
            continue;
        }
        pids.truncate(count as usize);
        break pids;
    };
    let mut result = Vec::new();
    for pid in pids.into_iter().filter(|pid| *pid > 0) {
        match coalition(pid) {
            Ok(id) if id == resource => match super::process_birthtime(pid) {
                Ok(started) if started != (0, 0) => result.push(Member { pid, started }),
                Ok(_) => {}
                Err(error) if gone(&error) => {}
                Err(error) => return Err(error),
            },
            Ok(_) => {}
            Err(error) if gone(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

fn gone(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| matches!(error.raw_os_error(), Some(libc::ESRCH | libc::ENOENT)))
}

fn terminate_coalition(resource: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let targets: Vec<_> = members(resource)?
            .into_iter()
            .filter(|member| member.pid != std::process::id() as i32)
            .collect();
        if targets.is_empty() {
            return Ok(());
        }
        for target in targets {
            if coalition(target.pid).ok() == Some(resource)
                && super::process_birthtime(target.pid).ok() == Some(target.started)
            {
                if unsafe { libc::kill(target.pid, libc::SIGKILL) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
        }
        if Instant::now() >= deadline {
            bail!("sandbox descendants did not terminate");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn send_descriptors(stream: &UnixStream) -> Result<()> {
    let mut byte = [1u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(12) };
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(12);
        std::ptr::copy_nonoverlapping([0i32, 1, 2].as_ptr(), libc::CMSG_DATA(header).cast(), 3);
        if libc::sendmsg(stream.as_raw_fd(), &message, 0) != 1 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn receive_descriptors(stream: &UnixStream) -> Result<[OwnedFd; 3]> {
    let mut byte = [0u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control) as _;
    unsafe {
        if libc::recvmsg(stream.as_raw_fd(), &mut message, 0) != 1
            || message.msg_flags & libc::MSG_CTRUNC != 0
        {
            bail!("invalid descriptor handoff");
        }
        let header = libc::CMSG_FIRSTHDR(&message);
        if header.is_null()
            || (*header).cmsg_level != libc::SOL_SOCKET
            || (*header).cmsg_type != libc::SCM_RIGHTS
            || (*header).cmsg_len != libc::CMSG_LEN(12)
        {
            bail!("missing stdio descriptors");
        }
        let descriptors = libc::CMSG_DATA(header).cast::<i32>();
        Ok([
            OwnedFd::from_raw_fd(*descriptors),
            OwnedFd::from_raw_fd(*descriptors.add(1)),
            OwnedFd::from_raw_fd(*descriptors.add(2)),
        ])
    }
}

fn send_json(stream: &mut UnixStream, value: &Value) -> Result<()> {
    stream.write_all(value.to_string().as_bytes())?;
    stream.write_all(b"\n")?;
    Ok(())
}
fn receive_json(stream: &mut UnixStream) -> Result<Value> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
        if bytes.len() > MAX_MESSAGE {
            bail!("oversized supervisor message");
        }
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn bootout(label: &str) {
    let _ = Command::new("/bin/launchctl")
        .args(["bootout", &format!("{}/{label}", domain())])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
fn permitted_environment() -> Vec<(String, String)> {
    const KEYS: &[&str] = &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "TMPDIR",
        "TMP",
        "TEMP",
        "LANG",
        "LC_ALL",
        "TZ",
        "TERM",
        "COLORTERM",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "GROK_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "RUSTUP_HOME",
        "CARGO_HOME",
        "NPM_CONFIG_CACHE",
        "PIP_CACHE_DIR",
        "GRADLE_USER_HOME",
        "TENTAFLOW_TOOLCHAIN_BASE",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "NO_PROXY",
        "no_proxy",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "NODE_EXTRA_CA_CERTS",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_USE_ENV_PROXY",
    ];
    KEYS.iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| ((*key).into(), value)))
        .collect()
}
fn xml(value: &str) -> Result<String> {
    if value
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        bail!("invalid XML argument");
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;"))
}
fn job_plist(label: &str, executable: &Path, spec: &Path) -> Result<String> {
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>{WORKER}</string><string>{}</string></array><key>RunAtLoad</key><true/><key>StandardInPath</key><string>/dev/null</string><key>StandardOutPath</key><string>/dev/null</string><key>StandardErrorPath</key><string>/dev/null</string></dict></plist>"#,
        xml(label)?,
        xml(&executable.display().to_string())?,
        xml(&spec.display().to_string())?
    ))
}
