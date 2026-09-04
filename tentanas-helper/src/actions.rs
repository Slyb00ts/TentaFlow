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
    arc_modprobe_file, block, nfs_conf_file, HelperCommand, NfsTransport, ARC_MAX_SYSFS_PATH,
    ARC_MODPROBE_PATH, AUDIT_RULES_PATH, KSMBD_CONF_PATH, KSMBD_LOCK_PATH, KSMBD_PWDDB_PATH,
    NFSD_PORTLIST_PATH, NFS_CONF_PATH, NFS_EXPORTS_PATH, NFS_RDMA_PORT, SHARE_GROUP,
    SMB_CONF_PATH, SMB_INCLUDE_PATH, SMB_MARKER_BEGIN, SMB_MARKER_END,
};

const TESTPARM: &[&str] = &["/usr/bin/testparm", "/usr/sbin/testparm"];
const SMBCONTROL: &[&str] = &["/usr/bin/smbcontrol", "/usr/sbin/smbcontrol"];
const SMBPASSWD: &[&str] = &["/usr/bin/smbpasswd", "/usr/sbin/smbpasswd"];
const EXPORTFS: &[&str] = &["/usr/sbin/exportfs", "/sbin/exportfs"];
const KSMBD_MOUNTD: &[&str] = &["/usr/sbin/ksmbd.mountd", "/sbin/ksmbd.mountd", "/usr/local/sbin/ksmbd.mountd"];
const KSMBD_CONTROL: &[&str] = &["/usr/sbin/ksmbd.control", "/sbin/ksmbd.control", "/usr/local/sbin/ksmbd.control"];
const KSMBD_ADDUSER: &[&str] = &["/usr/sbin/ksmbd.adduser", "/sbin/ksmbd.adduser", "/usr/local/sbin/ksmbd.adduser"];
const MODPROBE: &[&str] = &["/usr/sbin/modprobe", "/sbin/modprobe", "/usr/bin/modprobe"];
const MOUNT: &[&str] = &["/usr/bin/mount", "/bin/mount"];
const UMOUNT: &[&str] = &["/usr/bin/umount", "/bin/umount"];
const GROUPADD: &[&str] = &["/usr/sbin/groupadd", "/sbin/groupadd"];
const USERADD: &[&str] = &["/usr/sbin/useradd", "/sbin/useradd"];
const USERDEL: &[&str] = &["/usr/sbin/userdel", "/sbin/userdel"];
const NOLOGIN: &[&str] = &["/usr/sbin/nologin", "/sbin/nologin", "/bin/false"];
const AUGENRULES: &[&str] = &["/usr/sbin/augenrules", "/sbin/augenrules"];

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
        HelperCommand::KsmbdConfigWrite {} => ksmbd_config_write(payload),
        HelperCommand::KsmbdConfigClear {} => ksmbd_config_clear(),
        HelperCommand::KsmbdUserSet { user } => ksmbd_user_set(user, payload),
        HelperCommand::KsmbdUserDelete { user } => ksmbd_user_delete(user),
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
        HelperCommand::AuditRulesWrite {} => audit_rules_write(payload),
        HelperCommand::AuditRulesClear {} => audit_rules_clear(),
        HelperCommand::BlockModulesLoad { protocol } => block_modules_load(protocol),
        HelperCommand::IscsiTargetApply {} => iscsi_target_apply(payload),
        HelperCommand::IscsiTargetRemove { iqn } => {
            block::remove_iscsi(Path::new(block::TARGET_CONFIGFS), iqn).map(|log| log.join("\n"))
        }
        HelperCommand::NvmetSubsystemApply {} => nvmet_subsystem_apply(payload),
        HelperCommand::NvmetSubsystemRemove { nqn } => {
            block::remove_nvmet(Path::new(block::NVMET_CONFIGFS), nqn).map(|log| log.join("\n"))
        }
        HelperCommand::NvmetSessionsRead {} => nvmet_sessions_read(),
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

/// Puts the app's block at the END of the `[global]` section of `text`,
/// appending a `[global]` of its own when the file has none.
///
/// WHY the position matters (§5.4b): `interfaces` and `bind interfaces only`
/// are GLOBAL parameters, and the app-owned include now carries them whenever
/// ksmbd takes the RDMA interfaces. An include line sitting at the end of the
/// file — where this used to put it — lands inside whatever section came last
/// (`[homes]`, `[printers]`), so those two would configure a SHARE and the
/// listener split would silently not happen.
///
/// The END of `[global]` and not its start: an included file finishes in the
/// last section IT opened, so anything after the include line inside `[global]`
/// would end up in the app's last share section instead. At the end of the
/// section the next line of smb.conf is a section header, which resets the
/// parser by itself; a `[global]` that is the file's last section has nothing
/// after it at all.
fn place_include_block(text: &str) -> String {
    let (rest, _) = strip_block(text);
    let mut lines: Vec<&str> = rest.lines().collect();
    let global = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case("[global]"));
    let insert_at = match global {
        Some(start) => lines
            .iter()
            .skip(start + 1)
            .position(|l| l.trim().starts_with('['))
            .map(|offset| start + 1 + offset)
            .unwrap_or(lines.len()),
        None => {
            lines.push("[global]");
            lines.len()
        }
    };
    let block = include_block();
    let mut out = String::with_capacity(rest.len() + block.len() + 16);
    for line in &lines[..insert_at] {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&block);
    for line in &lines[insert_at..] {
        out.push_str(line);
        out.push('\n');
    }
    out
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
    let next = place_include_block(&text);
    // Comparing the whole rendered file is what MOVES a block an older version
    // of this helper left at the end of smb.conf, instead of reporting it as
    // already present in a place where its global parameters do nothing.
    if next == text {
        return Ok(format!("{SMB_CONF_PATH}: include block already in [global]"));
    }
    write_atomic(conf, next.as_bytes(), 0o644)?;
    Ok(format!("{SMB_CONF_PATH}: include block written into [global]"))
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

// ----- SMB Direct through ksmbd (§5.4b) ---------------------------------------------

/// The pid `ksmbd.mountd` wrote into its lock file, when that process is still
/// the daemon. ksmbd-tools has no status command and the node may have no
/// service manager, so its own lock file is the authoritative answer.
fn ksmbd_pid() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(KSMBD_LOCK_PATH)
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    (comm.trim() == "ksmbd.mountd").then_some(pid)
}

/// Waits until the daemon reaches `running`, up to three seconds. `mountd`
/// forks before it takes the lock and unlinks it while exiting, so both the
/// start and the restart would otherwise race their own verification.
fn ksmbd_wait(running: bool) -> bool {
    for _ in 0..60 {
        if ksmbd_pid().is_some() == running {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    ksmbd_pid().is_some() == running
}

/// The `[global]` section of a ksmbd config, normalized to its non-empty,
/// non-comment lines.
///
/// WHY: ksmbd.conf(5) is explicit that a change to a GLOBAL parameter takes
/// effect only after ksmbd.mountd and ksmbd are restarted, while
/// `ksmbd.control --reload` covers shares and users. `interfaces` and
/// `bind interfaces only` are global, so a listener change that was only
/// reloaded would leave ksmbd bound to the interfaces of the PREVIOUS config
/// — the one case this whole feature may not get wrong.
fn ksmbd_global_section(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = header.eq_ignore_ascii_case("global");
            continue;
        }
        if inside {
            out.push(line.to_string());
        }
    }
    out
}

/// Makes the config on disk the one ksmbd serves: a reload when only shares
/// and users changed, a full restart when the listener did.
fn ksmbd_apply(control: &Path, restart: bool, log: &mut Vec<String>) -> Result<(), String> {
    let running = ksmbd_pid().is_some();
    if running && !restart {
        let out = exec(control, &["--reload"], None)?;
        if !out.ok() {
            return Err(format!("ksmbd.control --reload failed: {}", out.output));
        }
        log.push("ksmbd reloaded".to_string());
        return Ok(());
    }
    if running {
        let out = exec(control, &["--shutdown"], None)?;
        if !out.ok() {
            return Err(format!("ksmbd.control --shutdown failed: {}", out.output));
        }
        if !ksmbd_wait(false) {
            return Err("ksmbd.mountd did not shut down".to_string());
        }
        log.push("ksmbd stopped: its listener changed".to_string());
    }
    // The unit ksmbd-tools ships pulls `modprobe@ksmbd` in BEFORE
    // ksmbd.mountd, because mountd does not load the kernel server itself. A
    // node driven from this catalog has no such unit, so the module is loaded
    // here or the daemon comes up unable to reach the server it configures.
    let modprobe = tool("modprobe", MODPROBE)?;
    let loaded = exec(&modprobe, &["ksmbd"], None)?;
    if !loaded.ok() {
        return Err(format!(
            "modprobe ksmbd failed: {} (does this kernel have CONFIG_SMB_SERVER?)",
            loaded.output
        ));
    }
    let mountd = tool("ksmbd.mountd", KSMBD_MOUNTD)?;
    let out = exec(&mountd, &[], None)?;
    if !out.ok() {
        return Err(format!("ksmbd.mountd failed to start: {}", out.output));
    }
    if !ksmbd_wait(true) {
        return Err("ksmbd.mountd exited without taking its lock file".to_string());
    }
    log.push(format!(
        "ksmbd started: TCP {} and SMB Direct {} on the bound interfaces",
        crate::KSMBD_TCP_PORT,
        crate::SMB_DIRECT_PORT
    ));
    Ok(())
}

fn ksmbd_config_write(payload: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(payload).map_err(|_| "ksmbd.conf is not UTF-8")?;
    // ksmbd-tools ship no dry-run parser, so this catalog's own is the gate;
    // it also enforces the §5.4b rule that the listener is bound to named
    // interfaces and never to the whole node.
    crate::validate_ksmbd_config(text).map_err(|e| e.to_string())?;
    let control = tool("ksmbd.control", KSMBD_CONTROL)?;
    let target = Path::new(KSMBD_CONF_PATH);
    let previous = snapshot(target);
    let restart = previous
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .map(|old| ksmbd_global_section(old) != ksmbd_global_section(text))
        .unwrap_or(true);
    write_atomic(target, payload, 0o644)?;

    let mut log = vec![format!("{KSMBD_CONF_PATH}: {} bytes written", payload.len())];
    if let Err(e) = ksmbd_apply(&control, restart, &mut log) {
        restore(target, previous.clone(), 0o644);
        // The daemon must end up serving whatever is on disk: putting the old
        // file back without re-applying it would leave ksmbd stopped (or on
        // the rejected listener) while the file says something else.
        if previous.is_some() {
            let mut back = Vec::new();
            let _ = ksmbd_apply(&control, restart, &mut back);
        }
        return Err(format!("{e}, ksmbd config rolled back"));
    }
    Ok(log.join("\n"))
}

fn ksmbd_config_clear() -> Result<String, String> {
    let mut log = Vec::new();
    if ksmbd_pid().is_some() {
        match tool("ksmbd.control", KSMBD_CONTROL) {
            Ok(control) => {
                let out = exec(&control, &["--shutdown"], None)?;
                log.push(format!("ksmbd.control --shutdown: exit {}", out.code));
                if !ksmbd_wait(false) {
                    log.push("ksmbd.mountd is still holding its lock file".to_string());
                }
            }
            // The teardown must not stop over a missing tool; removing the
            // config below is the part that has to happen.
            Err(e) => log.push(format!("ksmbd not shut down: {e}")),
        }
    } else {
        log.push("ksmbd is not running".to_string());
    }
    let target = Path::new(KSMBD_CONF_PATH);
    if target.exists() {
        std::fs::remove_file(target)
            .map_err(|e| format!("cannot remove {KSMBD_CONF_PATH}: {e}"))?;
        log.push(format!("{KSMBD_CONF_PATH}: removed"));
    } else {
        log.push(format!("{KSMBD_CONF_PATH}: not present"));
    }
    // The `ksmbd` module is left loaded on purpose: with the daemon down it
    // serves nothing, and unloading a module the admin may have loaded for
    // their own reasons is not this teardown's call.
    Ok(log.join("\n"))
}

// ----- share users ----------------------------------------------------------------

/// A password on its way to a tool's stdin: the new value and its
/// confirmation, which is what both `smbpasswd -s` and `ksmbd.adduser` read.
/// It is never an argv word, never logged and never written anywhere; the drop
/// zeroes the buffer so it does not linger in the wrapper's memory.
struct PasswordStdin(Vec<u8>);

impl PasswordStdin {
    fn new(payload: &[u8]) -> Result<Self, String> {
        if payload.is_empty() {
            return Err("no password on stdin".to_string());
        }
        let password = std::str::from_utf8(payload).map_err(|_| "password is not UTF-8")?;
        let password = password.trim_end_matches(['\n', '\r']);
        if password.is_empty() {
            return Err("empty password".to_string());
        }
        let mut bytes = Vec::with_capacity(password.len() * 2 + 2);
        for _ in 0..2 {
            bytes.extend_from_slice(password.as_bytes());
            bytes.push(b'\n');
        }
        Ok(Self(bytes))
    }
}

impl PasswordStdin {
    fn zeroize(&mut self) {
        self.0.iter_mut().for_each(|b| *b = 0);
    }
}

impl Drop for PasswordStdin {
    fn drop(&mut self) {
        self.zeroize();
    }
}

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
    let stdin = PasswordStdin::new(payload)?;
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
    let out = exec(&smbpasswd, &["-s", "-a", user], Some(&stdin.0))?;
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

/// The same account in ksmbd's own database, from the SAME password the Samba
/// entry got (§5.4b). ksmbd stores an MD4 hash next to the name in
/// `ksmbdpwd.db` and shares nothing with Samba's passdb, so one share account
/// working in both backends means writing it twice.
fn ksmbd_user_set(user: &str, payload: &[u8]) -> Result<String, String> {
    let stdin = PasswordStdin::new(payload)?;
    let adduser = tool("ksmbd.adduser", KSMBD_ADDUSER)?;
    // No `-a`/`-u`: given neither, ksmbd.adduser adds the account or updates
    // its password depending on what the database already holds, which is the
    // idempotent contract `smbpasswd -a` gives on the Samba side. It notifies
    // a running ksmbd.mountd itself.
    let out = exec(&adduser, &[user], Some(&stdin.0))?;
    if !out.ok() {
        return Err(format!("ksmbd.adduser {user} failed: {}", out.output));
    }
    Ok(format!("{KSMBD_PWDDB_PATH}: entry for {user} set"))
}

fn ksmbd_user_delete(user: &str) -> Result<String, String> {
    let adduser = tool("ksmbd.adduser", KSMBD_ADDUSER)?;
    let out = exec(&adduser, &["--delete", user], None)?;
    if !out.ok() {
        return Err(format!("ksmbd.adduser --delete {user} failed: {}", out.output));
    }
    // The POSIX account belongs to `SmbUserDelete`, which is called with this
    // one and owns it; removing it here would race that.
    Ok(format!("{KSMBD_PWDDB_PATH}: entry for {user} removed"))
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

// ----- auditd watches on the audited NFS exports (§5.10) ----------------------------

/// Writes the app-owned rules file and loads it. `augenrules --load` compiles
/// every drop-in in `/etc/audit/rules.d` and hands the result to the kernel, so
/// the file is validated and rolled back exactly like the exports file: a
/// rejected rule set must not stay behind on disk to be loaded at the next
/// boot, when nobody is watching.
fn audit_rules_write(payload: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(payload).map_err(|_| "audit rules file is not UTF-8")?;
    crate::validate_audit_rules(text).map_err(|e| e.to_string())?;
    let augenrules = tool("augenrules", AUGENRULES)?;
    let target = Path::new(AUDIT_RULES_PATH);
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let previous = snapshot(target);
    write_atomic(target, payload, 0o640)?;
    let applied = exec(&augenrules, &["--load"], None)?;
    if !applied.ok() {
        restore(target, previous, 0o640);
        let _ = exec(&augenrules, &["--load"], None);
        return Err(format!(
            "augenrules --load failed, audit rules rolled back: {}",
            applied.output
        ));
    }
    Ok(format!(
        "{AUDIT_RULES_PATH}: {} bytes written\naugenrules --load: ok\n{}",
        payload.len(),
        applied.output
    ))
}

// ----- block targets (§5.5) ---------------------------------------------------------

/// Whether configfs is mounted at all. Without it neither LIO nor nvmet can be
/// configured, and saying so beats a `cannot create …: No such file or
/// directory` from the first `mkdir` of a plan.
fn require_configfs(root: &str) -> Result<(), String> {
    if Path::new(root).is_dir() {
        return Ok(());
    }
    Err(format!(
        "{root} does not exist — run the block module load first"
    ))
}

/// The configfs mountpoint the kernel target subsystems publish under.
const CONFIGFS_MOUNT: &str = "/sys/kernel/config";

/// Loads the kernel target modules of one protocol and mounts configfs.
///
/// Nothing else on the node does this: §3.4 forbids enabling `target.service`
/// or `nvmet.service`, which is what would normally load them, because those
/// units restore a configuration from a second source of truth. Same shape as
/// the `modprobe ksmbd` step above, and the same reason.
///
/// Loading a module that is already loaded is success — this runs before every
/// first apply and on every restore.
fn block_modules_load(protocol: &str) -> Result<String, String> {
    // THE one list — see `block::modules_for`. The core's verdict ("can this
    // node serve the protocol at all?") reads the same function, so the module
    // set the probe checks for and the module set this loads cannot drift
    // apart.
    let modules = block::modules_for(protocol);
    if modules.is_empty() {
        return Err(format!("'{protocol}' is not a block protocol"));
    }
    let mut log = Vec::new();
    // configfs first: the modules create their trees under it when they load,
    // and a module loaded before the mount publishes nothing.
    if !Path::new(CONFIGFS_MOUNT).join("target").is_dir()
        && !Path::new(CONFIGFS_MOUNT).join("nvmet").is_dir()
    {
        let mount = tool("mount", MOUNT)?;
        let out = exec(
            &mount,
            &["-t", "configfs", "configfs", CONFIGFS_MOUNT],
            None,
        )?;
        // Already mounted is the common case and not an error; the module load
        // below is what actually decides whether this node can serve.
        log.push(if out.ok() {
            format!("{CONFIGFS_MOUNT} mounted")
        } else {
            format!("{CONFIGFS_MOUNT}: {}", out.output)
        });
    }
    let modprobe = tool("modprobe", MODPROBE)?;
    for module in modules {
        let out = exec(&modprobe, &[module], None)?;
        if !out.ok() {
            return Err(format!(
                "modprobe {module} failed: {} (does this kernel have the {protocol} target?)",
                out.output
            ));
        }
        log.push(format!("{module} loaded"));
    }
    let root = if protocol == "nvmet" {
        block::NVMET_CONFIGFS
    } else {
        block::TARGET_CONFIGFS
    };
    if !Path::new(root).is_dir() {
        return Err(format!(
            "{root} is still absent after loading {}",
            modules.join(", ")
        ));
    }
    log.push(format!("{root} present"));
    Ok(log.join("\n"))
}

/// Applies one iSCSI target from the spec on stdin.
///
/// The spec is validated by the catalog's own rules BEFORE any part of it
/// reaches the kernel, exactly like the two service-config writers: a plan
/// that is half-applied leaves a target answering with the wrong credentials,
/// and configfs has no transaction to roll back with.
fn iscsi_target_apply(payload: &[u8]) -> Result<String, String> {
    let spec: block::IscsiTargetSpec =
        serde_json::from_slice(payload).map_err(|e| format!("iSCSI target spec: {e}"))?;
    require_configfs(block::TARGET_CONFIGFS)?;
    // Observed HERE, not by the core: between a preview and this apply another
    // request may have changed what the kernel holds, and a plan built for the
    // wrong state either writes an attribute LIO refuses or removes an object
    // somebody else just created.
    let observed = block::observe_iscsi(Path::new(block::TARGET_CONFIGFS), &spec);
    let plan = block::plan_iscsi(&spec, &observed).map_err(|e| e.to_string())?;
    // The warnings are the credential-mode check (see `protect_attr`). They go
    // into the job log ABOVE the summary line, because a key that stayed
    // world-readable is the one thing about this apply an admin has to act on.
    let warnings = block::apply_plan(&plan)?;
    // The rendered plan goes into the job log — `render` is the only rendering
    // there is and it prints `***` for every secret.
    Ok(format!(
        "{}\n{}iSCSI target {} applied ({} configfs steps)",
        block::render(&plan).trim_end(),
        warnings.iter().map(|w| format!("{w}\n")).collect::<String>(),
        spec.iqn,
        block::kernel_step_count(&plan)
    ))
}

fn nvmet_subsystem_apply(payload: &[u8]) -> Result<String, String> {
    let spec: block::NvmetSubsystemSpec =
        serde_json::from_slice(payload).map_err(|e| format!("NVMe-oF subsystem spec: {e}"))?;
    require_configfs(block::NVMET_CONFIGFS)?;
    // Observed here and not by the core: a port is node-wide, and between the
    // preview and the apply another target may have taken one, freed one, or
    // enabled the one this subsystem is about to join.
    let observed = block::observe_nvmet(Path::new(block::NVMET_CONFIGFS), &spec);
    let plan = block::plan_nvmet(&spec, &observed).map_err(|e| e.to_string())?;
    let warnings = block::apply_plan(&plan)?;
    Ok(format!(
        "{}\n{}NVMe-oF subsystem {} applied ({} configfs steps)",
        block::render(&plan).trim_end(),
        warnings.iter().map(|w| format!("{w}\n")).collect::<String>(),
        spec.nqn,
        block::kernel_step_count(&plan)
    ))
}

/// Reports the NVMe-oF controllers attached to this node, as JSON on stdout.
///
/// ALWAYS `Ok`. "This kernel does not publish its controllers" is an ANSWER,
/// not a failure: the core has to be able to tell it apart from a call that
/// broke, because the two mean different things in the UI — a dash with a
/// reason versus a number nobody could measure. A non-zero exit would collapse
/// both into "no data".
///
/// Read-only: it never mounts debugfs and never loads a module, so a list poll
/// cannot reconfigure the node's kernel.
fn nvmet_sessions_read() -> Result<String, String> {
    let found = block::read_nvmet_sessions(Path::new(block::NVMET_DEBUGFS));
    serde_json::to_string(&found).map_err(|e| format!("nvmet sessions: {e}"))
}

fn audit_rules_clear() -> Result<String, String> {
    let target = Path::new(AUDIT_RULES_PATH);
    if !target.exists() {
        return Ok(format!("{AUDIT_RULES_PATH}: not present"));
    }
    std::fs::remove_file(target).map_err(|e| format!("cannot remove {AUDIT_RULES_PATH}: {e}"))?;
    // The watches live in the kernel until the rules are recompiled, so the
    // reload is part of the removal — leaving them loaded would keep auditing
    // paths the app no longer claims. A host without augenrules cannot have
    // had our watches loaded in the first place, so the file removal is the
    // whole job there.
    match tool("augenrules", AUGENRULES) {
        Ok(augenrules) => {
            let applied = exec(&augenrules, &["--load"], None)?;
            Ok(format!(
                "{AUDIT_RULES_PATH}: removed\naugenrules --load: {}",
                if applied.ok() { "ok" } else { &applied.output }
            ))
        }
        Err(e) => Ok(format!("{AUDIT_RULES_PATH}: removed ({e})")),
    }
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
    fn the_include_lands_at_the_end_of_the_global_section() {
        // The listener split of §5.4b puts `interfaces` and `bind interfaces
        // only` into the included file, and those are [global] parameters: an
        // include line appended to the END of smb.conf lands inside [homes]
        // and they would silently configure a share instead.
        let conf = "[global]\n   workgroup = WORKGROUP\n   security = user\n\n[homes]\n   browseable = no\n";
        let placed = place_include_block(conf);
        assert_eq!(
            placed,
            "[global]\n   workgroup = WORKGROUP\n   security = user\n\n\
             # BEGIN tentanas\n    include = /etc/samba/tentanas.conf\n# END tentanas\n\
             [homes]\n   browseable = no\n"
        );
        // Writing it twice changes nothing: the ensure step compares the whole
        // rendered file, so it is idempotent.
        assert_eq!(place_include_block(&placed), placed);

        // A block an older helper left at the end of the file is MOVED, not
        // reported as already present in a place where it does nothing.
        let misplaced = "[global]\n   workgroup = WORKGROUP\n\n[homes]\n   browseable = no\n# BEGIN tentanas\n    include = /etc/samba/tentanas.conf\n# END tentanas\n";
        let fixed = place_include_block(misplaced);
        assert!(fixed.find("# BEGIN tentanas") < fixed.find("[homes]"));
        assert_eq!(fixed.matches("# BEGIN tentanas").count(), 1);
        assert!(fixed.contains("   browseable = no\n"), "{fixed}");

        // [global] as the last section: nothing follows, so the block simply
        // ends the file and stays inside it.
        let only_global = "[global]\n   workgroup = WORKGROUP\n";
        let appended = place_include_block(only_global);
        assert_eq!(
            appended,
            "[global]\n   workgroup = WORKGROUP\n# BEGIN tentanas\n    include = /etc/samba/tentanas.conf\n# END tentanas\n"
        );

        // A smb.conf with no [global] at all gets one: a repeated [global]
        // section is a continuation of the first, so this is safe, and the
        // parameters have to have a global section to live in.
        let headless = "[printers]\n   printable = yes\n";
        let created = place_include_block(headless);
        assert_eq!(
            created,
            "[printers]\n   printable = yes\n[global]\n# BEGIN tentanas\n    include = /etc/samba/tentanas.conf\n# END tentanas\n"
        );
    }

    #[test]
    fn the_ksmbd_listener_decides_between_a_reload_and_a_restart() {
        // Only shares changed: ksmbd.control --reload covers those, and a
        // restart would drop every live SMB Direct session for nothing.
        let before = "[global]\n\tinterfaces = enp1s0f0np0\n\tbind interfaces only = yes\n[a]\n\tpath = /mnt/tank/a\n";
        let same_listener = "[global]\n\tinterfaces = enp1s0f0np0\n\tbind interfaces only = yes\n[a]\n\tpath = /mnt/tank/a\n[b]\n\tpath = /mnt/tank/b\n";
        assert_eq!(
            ksmbd_global_section(before),
            ksmbd_global_section(same_listener)
        );

        // The listener itself changed. ksmbd.conf(5) is explicit that a global
        // parameter only takes effect after a restart, so a reload here would
        // leave ksmbd bound to the PREVIOUS interface.
        let moved = "[global]\n\tinterfaces = enp1s0f1np1\n\tbind interfaces only = yes\n[a]\n\tpath = /mnt/tank/a\n";
        assert_ne!(ksmbd_global_section(before), ksmbd_global_section(moved));

        // Comments and blank lines are not a listener change.
        let commented = "# rewritten 2026-09-03\n\n[global]\n\n\tinterfaces = enp1s0f0np0\n; note\n\tbind interfaces only = yes\n[a]\n\tpath = /mnt/tank/a\n";
        assert_eq!(
            ksmbd_global_section(before),
            ksmbd_global_section(commented)
        );
        assert_eq!(
            ksmbd_global_section(before),
            vec![
                "interfaces = enp1s0f0np0".to_string(),
                "bind interfaces only = yes".to_string()
            ]
        );
    }

    #[test]
    fn the_password_buffer_is_the_double_entry_both_tools_read_and_is_zeroed() {
        let stdin = PasswordStdin::new(b"hunter2\n").expect("password");
        assert_eq!(stdin.0, b"hunter2\nhunter2\n");
        // `smbpasswd -s` and `ksmbd.adduser` both prompt twice, and neither
        // ever sees the secret in argv.
        assert!(PasswordStdin::new(b"").is_err());
        assert!(PasswordStdin::new(b"\n").is_err());
        assert!(PasswordStdin::new(&[0xff, 0xfe]).is_err());

        // What the drop runs: the buffer is zeroed in place, so the secret
        // does not linger in the wrapper's memory after the write.
        let mut owned = PasswordStdin::new(b"hunter2").expect("password");
        owned.zeroize();
        assert!(owned.0.iter().all(|b| *b == 0));
        assert_eq!(owned.0.len(), 16);
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
