// =============================================================================
// File: tentanas-helper/src/main.rs — the root-side wrapper of the TentaNas
// privilege channel (mode A). sudoers lets the core user run this binary
// without a password; the binary accepts ONE catalog command as a JSON line on
// stdin, validates it against the compiled-in catalog, logs it to syslog and
// execs the resolved argv with a sanitized environment. Anything else exits
// non-zero without running a thing.
//
// stdin framing: the first line is the command. Everything after it is a raw
// payload, accepted ONLY for the catalog entries that declare one
// (`reads_key_from_stdin`: a ZFS encryption key, a service config document, a
// Samba password) and forwarded to the child's stdin or to the builtin; for
// every other command a non-empty remainder is a protocol error, and the
// child's stdin stays closed.
//
// A builtin entry (`Plan::Builtin`) is performed by this process instead of
// being exec'd — see actions.rs for why a config write cannot be one command.
//
// Exit codes: 0..=255 of the child when it ran; 64 usage, 65 bad command,
// 66 tool missing, 67 not root, 68 spawn failure, 69 builtin action failed.
// =============================================================================

use std::ffi::CString;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, Stdio};

use tentanas_helper::{actions, CatalogError, HelperCommand, Plan, VERSION};

const MAX_COMMAND_BYTES: u64 = 16 * 1024;

fn syslog(message: &str) {
    // openlog/syslog are the only libc calls here; the identifier outlives
    // the process so a static C string is correct.
    static IDENT: &[u8] = b"tentanas-helper\0";
    let Ok(text) = CString::new(message.replace('\0', " ")) else {
        return;
    };
    unsafe {
        libc::openlog(
            IDENT.as_ptr() as *const libc::c_char,
            libc::LOG_PID,
            libc::LOG_AUTHPRIV,
        );
        libc::syslog(
            libc::LOG_NOTICE,
            b"%s\0".as_ptr() as *const libc::c_char,
            text.as_ptr(),
        );
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("{VERSION}");
        return ExitCode::SUCCESS;
    }
    if !args.is_empty() {
        eprintln!("usage: tentanas-helper [--version]  (command as a JSON line on stdin)");
        return ExitCode::from(64);
    }
    // Fail closed when sudo did not put us at uid 0: running the catalog as
    // the core user would only mask a broken channel.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("tentanas-helper: must run as root (via sudo)");
        return ExitCode::from(67);
    }

    let mut input = Vec::new();
    if let Err(e) = io::stdin().take(MAX_COMMAND_BYTES).read_to_end(&mut input) {
        eprintln!("tentanas-helper: cannot read command: {e}");
        return ExitCode::from(65);
    }
    let split = input.iter().position(|b| *b == b'\n').unwrap_or(input.len());
    let (line, rest) = input.split_at(split);
    let key_material = rest.strip_prefix(b"\n").unwrap_or(rest).to_vec();
    let Ok(line) = std::str::from_utf8(line) else {
        eprintln!("tentanas-helper: refused: command line is not UTF-8");
        return ExitCode::from(65);
    };
    let command: HelperCommand = match serde_json::from_str(line.trim()) {
        Ok(c) => c,
        Err(e) => {
            syslog(&format!("refused: unparseable command ({e})"));
            eprintln!("tentanas-helper: refused: {e}");
            return ExitCode::from(65);
        }
    };
    let wants_key = command.reads_key_from_stdin();
    if !wants_key && !key_material.is_empty() {
        syslog("refused: stdin payload for a command that takes none");
        eprintln!("tentanas-helper: refused: this command takes no stdin payload");
        return ExitCode::from(65);
    }
    if wants_key && key_material.is_empty() {
        syslog("refused: encryption command without key material");
        eprintln!("tentanas-helper: refused: no key material on stdin");
        return ExitCode::from(65);
    }
    let plan = match command.plan() {
        Ok(p) => p,
        Err(CatalogError::InvalidArgument(detail)) => {
            syslog(&format!("refused: {detail}"));
            eprintln!("tentanas-helper: refused: {detail}");
            return ExitCode::from(65);
        }
        Err(CatalogError::ToolMissing(tool)) => {
            syslog(&format!("refused: {tool} is not installed"));
            eprintln!("tentanas-helper: {tool} is not installed");
            return ExitCode::from(66);
        }
    };

    let caller_uid = unsafe { libc::getuid() };
    syslog(&format!("uid={caller_uid} exec: {}", plan.display()));

    let resolved = match plan {
        Plan::Exec(r) => r,
        Plan::Builtin(label) => {
            let mut payload = key_material;
            let outcome = actions::run(&command, &payload);
            payload.iter_mut().for_each(|b| *b = 0);
            return match outcome {
                Ok(log) => {
                    syslog(&format!("done: {label}"));
                    println!("{log}");
                    ExitCode::SUCCESS
                }
                Err(detail) => {
                    syslog(&format!("failed: {label}: {detail}"));
                    eprintln!("tentanas-helper: {label}: {detail}");
                    ExitCode::from(69)
                }
            };
        }
    };

    // Sanitized environment: nothing from the caller reaches the tool.
    let mut child = Command::new(&resolved.program);
    child
        .args(&resolved.args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .envs(resolved.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(if wants_key { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut spawned = match child.spawn() {
        Ok(c) => c,
        Err(e) => {
            syslog(&format!("spawn failed ({e}): {}", resolved.display()));
            eprintln!("tentanas-helper: spawn failed: {e}");
            return ExitCode::from(68);
        }
    };
    if wants_key {
        let mut key = key_material;
        if let Some(mut stdin) = spawned.stdin.take() {
            let write = stdin.write_all(&key).and_then(|()| stdin.flush());
            drop(stdin);
            if let Err(e) = write {
                key.iter_mut().for_each(|b| *b = 0);
                let _ = spawned.kill();
                syslog(&format!("key write failed ({e}): {}", resolved.display()));
                eprintln!("tentanas-helper: cannot pass the key to {}", resolved.display());
                return ExitCode::from(68);
            }
        }
        key.iter_mut().for_each(|b| *b = 0);
    }
    match spawned.wait() {
        Ok(status) => {
            let code = status.code().unwrap_or(1);
            syslog(&format!("exit={code}: {}", resolved.display()));
            ExitCode::from(code.clamp(0, 255) as u8)
        }
        Err(e) => {
            syslog(&format!("wait failed ({e}): {}", resolved.display()));
            eprintln!("tentanas-helper: wait failed: {e}");
            ExitCode::from(68)
        }
    }
}
