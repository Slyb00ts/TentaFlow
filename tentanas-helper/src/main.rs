// =============================================================================
// File: tentanas-helper/src/main.rs — the root-side wrapper of the TentaNas
// privilege channel (mode A). sudoers lets the core user run this binary
// without a password; the binary accepts ONE catalog command as a JSON line on
// stdin, validates it against the compiled-in catalog, logs it to syslog and
// execs the resolved argv with a sanitized environment. Anything else exits
// non-zero without running a thing.
//
// Exit codes: 0..=255 of the child when it ran; 64 usage, 65 bad command,
// 66 tool missing, 67 not root, 68 spawn failure.
// =============================================================================

use std::ffi::CString;
use std::io::{self, Read};
use std::process::{Command, ExitCode, Stdio};

use tentanas_helper::{CatalogError, HelperCommand, VERSION};

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

    let mut line = String::new();
    if let Err(e) = io::stdin().take(MAX_COMMAND_BYTES).read_to_string(&mut line) {
        eprintln!("tentanas-helper: cannot read command: {e}");
        return ExitCode::from(65);
    }
    let command: HelperCommand = match serde_json::from_str(line.trim()) {
        Ok(c) => c,
        Err(e) => {
            syslog(&format!("refused: unparseable command ({e})"));
            eprintln!("tentanas-helper: refused: {e}");
            return ExitCode::from(65);
        }
    };
    let resolved = match command.resolve() {
        Ok(r) => r,
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
    syslog(&format!("uid={caller_uid} exec: {}", resolved.display()));

    // Sanitized environment: nothing from the caller reaches the tool.
    let mut child = Command::new(&resolved.program);
    child
        .args(&resolved.args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .envs(resolved.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match child.status() {
        Ok(status) => {
            let code = status.code().unwrap_or(1);
            syslog(&format!("exit={code}: {}", resolved.display()));
            ExitCode::from(code.clamp(0, 255) as u8)
        }
        Err(e) => {
            syslog(&format!("spawn failed ({e}): {}", resolved.display()));
            eprintln!("tentanas-helper: spawn failed: {e}");
            ExitCode::from(68)
        }
    }
}
