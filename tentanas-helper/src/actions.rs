// =============================================================================
// File: tentanas-helper/src/actions.rs — the builtin catalog entries, i.e. the
// ones the root-side wrapper performs ITSELF instead of exec'ing one program.
//
// WHY: writing a service config is not a command, it is a transaction —
// validate the candidate, write it next to the target, let the service's own
// parser judge it, rename it into place, reload, verify, and put the previous
// content back when any of that fails. Splitting that into several sudo calls
// would let the channel stop half-way and leave smbd or nfsd exporting a
// config nobody wrote. So the sequence lives here, behind the same catalog
// validation as every exec entry, and runs as one privileged step.
//
// Everything below assumes it runs as root (main.rs refuses otherwise) and
// touches ONLY the paths the catalog owns: the app's include file, the marker
// block in smb.conf, the app's exports file, share roots under /mnt and fleet
// mountpoints under /mnt/tentanas.
// =============================================================================

use std::ffi::CString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{
    arc_modprobe_file, nfs_conf_file, HelperCommand, NfsTransport, ARC_MAX_SYSFS_PATH,
    ARC_MODPROBE_PATH, NFSD_PORTLIST_PATH, NFS_CONF_PATH, NFS_EXPORTS_PATH, NFS_RDMA_PORT,
    SHARE_GROUP, SMB_CONF_PATH, SMB_INCLUDE_PATH, SMB_MARKER_BEGIN, SMB_MARKER_END,
};

const TESTPARM: &[&str] = &["/usr/bin/testparm", "/usr/sbin/testparm"];
const SMBCONTROL: &[&str] = &["/usr/bin/smbcontrol", "/usr/sbin/smbcontrol"];
const SMBPASSWD: &[&str] = &["/usr/bin/smbpasswd", "/usr/sbin/smbpasswd"];
const EXPORTFS: &[&str] = &["/usr/sbin/exportfs", "/sbin/exportfs"];
const MOUNT: &[&str] = &["/usr/bin/mount", "/bin/mount"];
const UMOUNT: &[&str] = &["/usr/bin/umount", "/bin/umount"];
const GROUPADD: &[&str] = &["/usr/sbin/groupadd", "/sbin/groupadd"];
const USERADD: &[&str] = &["/usr/sbin/useradd", "/sbin/useradd"];
const USERDEL: &[&str] = &["/usr/sbin/userdel", "/sbin/userdel"];
const NOLOGIN: &[&str] = &["/usr/sbin/nologin", "/sbin/nologin", "/bin/false"];

/// Runs one builtin. `Ok` carries the log the wrapper prints on stdout, `Err`
/// the one-line reason it prints on stderr.
pub fn run(command: &HelperCommand, payload: &[u8]) -> Result<String, String> {
    match command {
        HelperCommand::SmbIncludeEnsure {} => smb_include_ensure(),
        HelperCommand::SmbIncludeRemove {} => smb_include_remove(),
        HelperCommand::SmbConfigWrite {} => smb_config_write(payload),
        HelperCommand::NfsExportsWrite {} => nfs_exports_write(payload),
        HelperCommand::NfsRdmaSet {} => nfs_rdma_set(),
        HelperCommand::NfsRdmaClear {} => nfs_rdma_clear(),
        HelperCommand::SmbUserSet { user } => smb_user_set(user, payload),
        HelperCommand::SmbUserDelete { user } => smb_user_delete(user),
        HelperCommand::ShareChown { path, guests } => share_chown(path, *guests),
        HelperCommand::FleetMount {
            source,
            export_path,
            mountpoint,
            transport,
        } => fleet_mount(source, export_path, mountpoint, *transport),
        HelperCommand::FleetUmount { mountpoint } => fleet_umount(mountpoint),
        HelperCommand::ArcLimitSet { max_bytes } => arc_limit_set(*max_bytes),
        HelperCommand::ArcLimitClear {} => arc_limit_clear(),
        other => Err(format!("{other:?} is not a builtin")),
    }
}

// ----- plumbing ------------------------------------------------------------------

fn tool(name: &str, candidates: &[&str]) -> Result<PathBuf, String> {
    candidates
        .iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{name} is not installed"))
}

struct Exit {
    code: i32,
    output: String,
}

impl Exit {
    fn ok(&self) -> bool {
        self.code == 0
    }
}

/// Runs a tool with a sanitized environment and captures both streams. Nothing
/// from the caller's environment reaches it — same rule as the exec path.
fn exec(program: &Path, args: &[&str], stdin_data: Option<&[u8]>) -> Result<Exit, String> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    if let Some(data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            let write = stdin.write_all(data).and_then(|()| stdin.flush());
            drop(stdin);
            write.map_err(|e| format!("cannot write to {}: {e}", program.display()))?;
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{} failed: {e}", program.display()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(Exit {
        code: out.status.code().unwrap_or(-1),
        output: text.trim().to_string(),
    })
}

/// Writes `content` into `target` through a temp file in the SAME directory
/// followed by a rename, so a reader never sees a half-written config.
fn write_atomic(target: &Path, content: &[u8], mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no directory", target.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let mut candidate = target.as_os_str().to_os_string();
    candidate.push(".tentanas-new");
    let candidate = PathBuf::from(candidate);
    std::fs::write(&candidate, content)
        .map_err(|e| format!("cannot write {}: {e}", candidate.display()))?;
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("cannot set the mode of {}: {e}", candidate.display()))?;
    std::fs::rename(&candidate, target).map_err(|e| {
        let _ = std::fs::remove_file(&candidate);
        format!("cannot replace {}: {e}", target.display())
    })
}

/// The previous content of a file the builtin is about to replace, so a failed
/// reload can put exactly that back. `None` = the file did not exist.
fn snapshot(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

fn restore(path: &Path, previous: Option<Vec<u8>>, mode: u32) {
    match previous {
        Some(bytes) => {
            let _ = write_atomic(path, &bytes, mode);
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ----- smb.conf marker block ------------------------------------------------------

fn include_block() -> String {
    format!("{SMB_MARKER_BEGIN}\n    include = {SMB_INCLUDE_PATH}\n{SMB_MARKER_END}\n")
}

/// Drops the app's block from `text`, leaving every other line untouched.
/// Returns the remainder and whether a block was found.
fn strip_block(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    let mut found = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == SMB_MARKER_BEGIN {
            inside = true;
            found = true;
            continue;
        }
        if inside {
            if trimmed == SMB_MARKER_END {
                inside = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    (out, found)
}

fn smb_include_ensure() -> Result<String, String> {
    let conf = Path::new(SMB_CONF_PATH);
    let text = std::fs::read_to_string(conf)
        .map_err(|e| format!("cannot read {SMB_CONF_PATH}: {e} (is samba installed?)"))?;
    // The app's own include file must exist before smbd parses the line: an
    // include of a missing file is silently ignored, which would look like the
    // shares vanished rather than like a broken write.
    if !Path::new(SMB_INCLUDE_PATH).exists() {
        write_atomic(Path::new(SMB_INCLUDE_PATH), b"", 0o644)?;
    }
    let (rest, found) = strip_block(&text);
    if found && text.contains(&format!("include = {SMB_INCLUDE_PATH}")) {
        return Ok(format!("{SMB_CONF_PATH}: include block already present"));
    }
    let mut next = rest;
    if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&include_block());
    write_atomic(conf, next.as_bytes(), 0o644)?;
    Ok(format!("{SMB_CONF_PATH}: include block written"))
}

fn smb_include_remove() -> Result<String, String> {
    let conf = Path::new(SMB_CONF_PATH);
    let mut log = Vec::new();
    if let Ok(text) = std::fs::read_to_string(conf) {
        let (rest, found) = strip_block(&text);
        if found {
            write_atomic(conf, rest.as_bytes(), 0o644)?;
            log.push(format!("{SMB_CONF_PATH}: include block removed"));
        }
    }
    if Path::new(SMB_INCLUDE_PATH).exists() {
        std::fs::remove_file(SMB_INCLUDE_PATH)
            .map_err(|e| format!("cannot remove {SMB_INCLUDE_PATH}: {e}"))?;
        log.push(format!("{SMB_INCLUDE_PATH}: removed"));
    }
    if let Ok(p) = tool("smbcontrol", SMBCONTROL) {
        let out = exec(&p, &["all", "reload-config"], None)?;
        log.push(format!("smbcontrol reload-config: exit {}", out.code));
    }
    if log.is_empty() {
        log.push("nothing to remove".to_string());
    }
    Ok(log.join("\n"))
}

// ----- service configs ------------------------------------------------------------

fn smb_config_write(payload: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(payload).map_err(|_| "smb.conf fragment is not UTF-8")?;
    crate::validate_smb_config(text).map_err(|e| e.to_string())?;
    let target = Path::new(SMB_INCLUDE_PATH);
    let testparm = tool("testparm", TESTPARM)?;
    // The candidate is judged by Samba's own parser before it can become the
    // live file: a fragment that testparm rejects would take every share with
    // it on the next reload.
    let candidate = target.with_extension("tentanas-candidate");
    write_atomic(&candidate, payload, 0o644)?;
    let check = exec(
        &testparm,
        &["-s", "--suppress-prompt", &candidate.display().to_string()],
        None,
    );
    let check = match check {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&candidate);
            return Err(e);
        }
    };
    if !check.ok() {
        let _ = std::fs::remove_file(&candidate);
        return Err(format!("testparm rejected the share config: {}", check.output));
    }
    let previous = snapshot(target);
    std::fs::rename(&candidate, target).map_err(|e| {
        let _ = std::fs::remove_file(&candidate);
        format!("cannot replace {SMB_INCLUDE_PATH}: {e}")
    })?;
    let mut log = vec![format!("{SMB_INCLUDE_PATH}: {} bytes written", payload.len())];
    match tool("smbcontrol", SMBCONTROL) {
        Ok(p) => {
            let out = exec(&p, &["all", "reload-config"], None)?;
            // A stopped smbd cannot be told to reload, and that is not a reason
            // to throw away a config testparm just accepted — it becomes live
            // when the service starts.
            log.push(if out.ok() {
                "smbd reloaded".to_string()
            } else {
                format!("smbd not reloaded (exit {}): {}", out.code, out.output)
            });
        }
        Err(e) => {
            restore(target, previous, 0o644);
            return Err(e);
        }
    }
    Ok(log.join("\n"))
}

fn nfs_exports_write(payload: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(payload).map_err(|_| "exports file is not UTF-8")?;
    for line in text.lines() {
        crate::validate_export_line(line).map_err(|e| e.to_string())?;
    }
    let exportfs = tool("exportfs", EXPORTFS)?;
    let target = Path::new(NFS_EXPORTS_PATH);
    let previous = snapshot(target);
    write_atomic(target, payload, 0o644)?;
    let applied = exec(&exportfs, &["-ra"], None)?;
    if !applied.ok() {
        restore(target, previous, 0o644);
        let _ = exec(&exportfs, &["-ra"], None);
        return Err(format!("exportfs -ra failed, exports rolled back: {}", applied.output));
    }
    let state = exec(&exportfs, &["-s"], None)?;
    Ok(format!(
        "{NFS_EXPORTS_PATH}: {} bytes written\nexportfs -ra: ok\n{}",
        payload.len(),
        state.output
    ))
}

// ----- NFS over RDMA (§5.5a) --------------------------------------------------------

/// The `<transport> <port>` line the portlist uses for our RDMA listener.
fn rdma_portlist_line() -> String {
    format!("rdma {NFS_RDMA_PORT}")
}

/// Adds or removes the RDMA listener of a RUNNING nfsd. A stopped nfsd has no
/// portlist at all, and that is not a failure: the drop-in is what its next
/// start reads.
fn set_rdma_listener(enabled: bool) -> Result<String, String> {
    let path = Path::new(NFSD_PORTLIST_PATH);
    let Ok(current) = std::fs::read_to_string(path) else {
        return Ok(format!(
            "{NFSD_PORTLIST_PATH}: nfsd is not running, the drop-in applies at its next start"
        ));
    };
    let wanted = rdma_portlist_line();
    let listening = current.lines().any(|l| l.trim() == wanted);
    if listening == enabled {
        return Ok(format!(
            "{NFSD_PORTLIST_PATH}: already {}",
            if enabled { "listening on rdma" } else { "without rdma" }
        ));
    }
    let line = if enabled {
        format!("{wanted}\n")
    } else {
        format!("-{wanted}\n")
    };
    // A procfs control file takes one write of the whole command; truncating
    // it (what `fs::write` would do) is not part of its contract.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| format!("cannot open {NFSD_PORTLIST_PATH}: {e}"))?;
    file.write_all(line.as_bytes()).map_err(|e| {
        format!(
            "cannot {} the rdma listener on {NFSD_PORTLIST_PATH}: {e} \
             (is the rpcrdma module available and an RDMA device present?)",
            if enabled { "add" } else { "remove" }
        )
    })?;
    Ok(format!(
        "{NFSD_PORTLIST_PATH}: rdma listener on port {NFS_RDMA_PORT} {}",
        if enabled { "added" } else { "removed" }
    ))
}

/// Persists the transport decision AND applies it to the running server. The
/// file is written first so a crash between the two leaves a node that comes
/// back with the transport the app decided, and it is rolled back when the
/// live change is refused so the persisted state never promises a listener
/// nfsd could not open.
fn nfs_rdma_set() -> Result<String, String> {
    let content = nfs_conf_file();
    crate::validate_nfs_conf(&content).map_err(|e| e.to_string())?;
    let target = Path::new(NFS_CONF_PATH);
    let previous = snapshot(target);
    write_atomic(target, content.as_bytes(), 0o644)?;
    match set_rdma_listener(true) {
        Ok(note) => Ok(format!(
            "{NFS_CONF_PATH}: {} bytes written\n{note}",
            content.len()
        )),
        Err(e) => {
            restore(target, previous, 0o644);
            Err(e)
        }
    }
}

fn nfs_rdma_clear() -> Result<String, String> {
    let mut log = Vec::new();
    match set_rdma_listener(false) {
        Ok(note) => log.push(note),
        // The uninstall must not stop over a listener the kernel already
        // dropped; the file removal below is the part that has to happen.
        Err(e) => log.push(format!("rdma listener not removed: {e}")),
    }
    let target = Path::new(NFS_CONF_PATH);
    if target.exists() {
        std::fs::remove_file(target)
            .map_err(|e| format!("cannot remove {NFS_CONF_PATH}: {e}"))?;
        log.push(format!("{NFS_CONF_PATH}: removed"));
    } else {
        log.push(format!("{NFS_CONF_PATH}: not present"));
    }
    Ok(log.join("\n"))
}

// ----- share users ----------------------------------------------------------------

fn user_exists(user: &str) -> bool {
    let Ok(name) = CString::new(user) else {
        return false;
    };
    // getpwnam reads whatever NSS is configured, which is what useradd would
    // collide with — /etc/passwd alone would miss an LDAP account.
    !unsafe { libc::getpwnam(name.as_ptr()) }.is_null()
}

fn ensure_group() -> Result<(), String> {
    let Ok(name) = CString::new(SHARE_GROUP) else {
        return Err("share group name".to_string());
    };
    if !unsafe { libc::getgrnam(name.as_ptr()) }.is_null() {
        return Ok(());
    }
    let groupadd = tool("groupadd", GROUPADD)?;
    let out = exec(&groupadd, &["-r", "-f", SHARE_GROUP], None)?;
    if out.ok() {
        Ok(())
    } else {
        Err(format!("groupadd {SHARE_GROUP} failed: {}", out.output))
    }
}

fn smb_user_set(user: &str, payload: &[u8]) -> Result<String, String> {
    if payload.is_empty() {
        return Err("no password on stdin".to_string());
    }
    let smbpasswd = tool("smbpasswd", SMBPASSWD)?;
    let mut log = Vec::new();
    ensure_group()?;
    if !user_exists(user) {
        let useradd = tool("useradd", USERADD)?;
        let shell = tool("nologin", NOLOGIN)?;
        let shell = shell.display().to_string();
        // -r -M -N: a system account with no home and no per-user group. The
        // account exists only so Samba has a POSIX identity to map to; it can
        // never log in.
        let out = exec(
            &useradd,
            &["-r", "-M", "-N", "-g", SHARE_GROUP, "-s", &shell, user],
            None,
        )?;
        if !out.ok() {
            return Err(format!("useradd {user} failed: {}", out.output));
        }
        log.push(format!("system account {user} created in {SHARE_GROUP}"));
    }
    // `smbpasswd -s` reads the new password twice; it never appears in argv,
    // so it stays out of `ps`, the job log and the syslog audit line.
    let password = std::str::from_utf8(payload).map_err(|_| "password is not UTF-8")?;
    let password = password.trim_end_matches(['\n', '\r']);
    if password.is_empty() {
        return Err("empty password".to_string());
    }
    let mut stdin = Vec::with_capacity(password.len() * 2 + 2);
    stdin.extend_from_slice(password.as_bytes());
    stdin.push(b'\n');
    stdin.extend_from_slice(password.as_bytes());
    stdin.push(b'\n');
    let out = exec(&smbpasswd, &["-s", "-a", user], Some(&stdin))?;
    stdin.iter_mut().for_each(|b| *b = 0);
    if !out.ok() {
        return Err(format!("smbpasswd -a {user} failed: {}", out.output));
    }
    let enabled = exec(&smbpasswd, &["-e", user], None)?;
    if !enabled.ok() {
        return Err(format!("smbpasswd -e {user} failed: {}", enabled.output));
    }
    log.push(format!("passdb entry for {user} set and enabled"));
    Ok(log.join("\n"))
}

fn smb_user_delete(user: &str) -> Result<String, String> {
    let mut log = Vec::new();
    if let Ok(smbpasswd) = tool("smbpasswd", SMBPASSWD) {
        let out = exec(&smbpasswd, &["-x", user], None)?;
        log.push(format!("smbpasswd -x {user}: exit {}", out.code));
    }
    if user_exists(user) {
        let userdel = tool("userdel", USERDEL)?;
        let out = exec(&userdel, &["-f", user], None)?;
        if !out.ok() {
            return Err(format!("userdel {user} failed: {}", out.output));
        }
        log.push(format!("system account {user} removed"));
    }
    Ok(log.join("\n"))
}

// ----- share root ownership --------------------------------------------------------

fn share_chown(path: &str, guests: bool) -> Result<String, String> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return Err(format!("{path} is not a directory"));
    }
    ensure_group()?;
    let group = CString::new(SHARE_GROUP).map_err(|_| "share group name")?;
    let gid = {
        let entry = unsafe { libc::getgrnam(group.as_ptr()) };
        if entry.is_null() {
            return Err(format!("group {SHARE_GROUP} does not exist"));
        }
        unsafe { (*entry).gr_gid }
    };
    let c_path = CString::new(path).map_err(|_| "share path")?;
    // The owner is left alone: only the group changes, so a dataset that
    // belongs to a real user keeps belonging to them.
    if unsafe { libc::chown(c_path.as_ptr(), u32::MAX, gid) } != 0 {
        return Err(format!(
            "cannot set the group of {path}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // setgid so everything created inside inherits the group the grants are
    // written against; guests connect as a user outside it, hence 2775.
    let mode: libc::mode_t = if guests { 0o2775 } else { 0o2770 };
    if unsafe { libc::chmod(c_path.as_ptr(), mode) } != 0 {
        return Err(format!(
            "cannot set the mode of {path}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(format!("{path}: group {SHARE_GROUP}, mode {mode:04o}"))
}

// ----- fleet mounts ----------------------------------------------------------------

/// Whether `path` is a mountpoint right now, read from the kernel's own list.
fn is_mounted(path: &str) -> bool {
    let Ok(text) = std::fs::read_to_string("/proc/self/mounts") else {
        return false;
    };
    text.lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .any(|m| m == path)
}

fn fleet_mount(
    source: &str,
    export_path: &str,
    mountpoint: &str,
    transport: NfsTransport,
) -> Result<String, String> {
    if is_mounted(mountpoint) {
        return Ok(format!("{mountpoint}: already mounted"));
    }
    let mount = tool("mount", MOUNT)?;
    std::fs::create_dir_all(mountpoint)
        .map_err(|e| format!("cannot create {mountpoint}: {e}"))?;
    // An IPv6 literal needs brackets or `mount` reads the colons as the
    // host/path separator.
    let spec = if source.contains(':') {
        format!("[{source}]:{export_path}")
    } else {
        format!("{source}:{export_path}")
    };
    let options = transport.mount_options();
    let out = exec(&mount, &["-t", "nfs", "-o", &options, &spec, mountpoint], None)?;
    if !out.ok() {
        // A mountpoint we just created and could not use would otherwise stay
        // behind as an empty, writable directory under /mnt/tentanas.
        let _ = std::fs::remove_dir(mountpoint);
        return Err(format!("mount -o {options} {spec} failed: {}", out.output));
    }
    Ok(format!(
        "{mountpoint}: mounted from {spec} over {}",
        transport.as_str()
    ))
}

fn fleet_umount(mountpoint: &str) -> Result<String, String> {
    let mut log = Vec::new();
    if is_mounted(mountpoint) {
        let umount = tool("umount", UMOUNT)?;
        let out = exec(&umount, &[mountpoint], None)?;
        if !out.ok() {
            return Err(format!("umount {mountpoint} failed: {}", out.output));
        }
        log.push(format!("{mountpoint}: unmounted"));
    }
    if Path::new(mountpoint).is_dir() && std::fs::remove_dir(mountpoint).is_ok() {
        log.push(format!("{mountpoint}: removed"));
    }
    if log.is_empty() {
        log.push(format!("{mountpoint}: not mounted"));
    }
    Ok(log.join("\n"))
}

// ----- the ARC limit ----------------------------------------------------------------

/// Caps the ARC now and for the next boot. The runtime write comes first: it
/// is the one the admin sees take effect, and a drop-in that a reboot would
/// apply while the running module ignores it is exactly the confusion this
/// builtin exists to prevent. The drop-in is written second and rolled back
/// when it fails, so the persisted value never disagrees with a write that
/// did not happen.
fn arc_limit_set(max_bytes: u64) -> Result<String, String> {
    let sysfs = Path::new(ARC_MAX_SYSFS_PATH);
    if !sysfs.exists() {
        return Err(format!("{ARC_MAX_SYSFS_PATH} does not exist (is the zfs module loaded?)"));
    }
    // A module parameter is not a regular file: it takes one write of the
    // decimal value and rejects anything else, so no atomic rename here.
    std::fs::write(sysfs, format!("{max_bytes}"))
        .map_err(|e| format!("cannot write {ARC_MAX_SYSFS_PATH}: {e}"))?;
    let target = Path::new(ARC_MODPROBE_PATH);
    let previous = snapshot(target);
    let content = arc_modprobe_file(max_bytes);
    if let Err(e) = write_atomic(target, content.as_bytes(), 0o644) {
        restore(target, previous, 0o644);
        return Err(e);
    }
    Ok(format!(
        "{ARC_MAX_SYSFS_PATH}: {max_bytes}\n{ARC_MODPROBE_PATH}: {} bytes written",
        content.len()
    ))
}

fn arc_limit_clear() -> Result<String, String> {
    let target = Path::new(ARC_MODPROBE_PATH);
    if !target.exists() {
        return Ok(format!("{ARC_MODPROBE_PATH}: not present"));
    }
    std::fs::remove_file(target).map_err(|e| format!("cannot remove {ARC_MODPROBE_PATH}: {e}"))?;
    // The running module keeps the cap until the next boot on purpose: the
    // kernel's default depends on the RAM at boot time and guessing it here
    // would be a worse surprise than an ARC that stays where the admin put it.
    Ok(format!("{ARC_MODPROBE_PATH}: removed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_block_is_removed_without_touching_other_lines() {
        let text = "[global]\n   workgroup = WORKGROUP\n\n# BEGIN tentanas\n    include = /etc/samba/tentanas.conf\n# END tentanas\n[homes]\n   browseable = no\n";
        let (rest, found) = strip_block(text);
        assert!(found);
        assert_eq!(
            rest,
            "[global]\n   workgroup = WORKGROUP\n\n[homes]\n   browseable = no\n"
        );
        // A file without the block comes back unchanged and reports it.
        let (again, found) = strip_block(&rest);
        assert!(!found);
        assert_eq!(again, rest);
    }

    #[test]
    fn the_include_block_names_exactly_the_app_owned_file() {
        assert_eq!(
            include_block(),
            "# BEGIN tentanas\n    include = /etc/samba/tentanas.conf\n# END tentanas\n"
        );
    }

    #[test]
    fn a_builtin_dispatch_refuses_an_exec_entry() {
        let out = run(&HelperCommand::ZpoolImportScan {}, b"");
        assert!(out.is_err());
    }

    #[test]
    fn the_arc_drop_in_is_one_options_line_the_app_owns() {
        let text = arc_modprobe_file(8 * 1024 * 1024 * 1024);
        assert!(text.starts_with("# Managed by TentaFlow TentaNas"));
        assert!(text.ends_with("options zfs zfs_arc_max=8589934592\n"));
        // Exactly one directive: modprobe reads every `options zfs` line, so a
        // second one would silently win or lose depending on file order.
        assert_eq!(text.lines().filter(|l| l.starts_with("options ")).count(), 1);
    }

    #[test]
    fn the_rdma_portlist_line_is_the_transport_and_the_iana_port() {
        assert_eq!(rdma_portlist_line(), "rdma 20049");
    }

    #[test]
    fn the_rdma_listener_is_a_no_op_when_nfsd_is_not_running() {
        // A node without nfsd has no portlist; the drop-in is the whole job
        // there and the builtin must not report that as a failure.
        if !Path::new(NFSD_PORTLIST_PATH).exists() {
            let out = set_rdma_listener(true).expect("no portlist is not an error");
            assert!(out.contains("not running"), "{out}");
        }
    }

    #[test]
    fn clearing_an_absent_arc_drop_in_is_not_an_error() {
        // The uninstall runs this unconditionally; a node that never set a
        // limit must not fail the teardown over a file that was never there.
        if !Path::new(ARC_MODPROBE_PATH).exists() {
            assert!(arc_limit_clear().is_ok());
        }
    }
}
