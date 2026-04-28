// =============================================================================
// File: profiling/elevation_runner.rs — single source of truth for spawning
// elevated commands (sudo on POSIX, UAC on Windows). Collectors call into
// this helper instead of building their own `Command::new("sudo")` so password
// handling, kind validation and headless detection live in one place.
// =============================================================================

use std::process::Stdio;

use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::profiling::collectors::elevation::{ElevationKind, ElevationToken};

#[derive(Error, Debug)]
pub enum ElevationError {
    #[error("invalid password")]
    InvalidPassword,
    #[error("sudo binary not found")]
    SudoNotFound,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("headless mode: cannot trigger UAC prompt")]
    HeadlessNotSupported,
    #[error("kind mismatch: expected {expected}, got {actual}")]
    KindMismatch {
        expected: &'static str,
        actual: &'static str,
    },
    #[error("{0}")]
    Other(String),
}

fn kind_label(k: ElevationKind) -> &'static str {
    match k {
        ElevationKind::None => "None",
        ElevationKind::Sudo => "Sudo",
        ElevationKind::Admin => "Admin",
        ElevationKind::LinuxCap => "LinuxCap",
    }
}

/// One-stop helper for elevated subprocesses. All methods are static — there is
/// no shared state; the elevation token is supplied per call.
pub struct ElevationRunner;

impl ElevationRunner {
    /// Validate a sudo password by running `sudo -k` (clear cached creds) then
    /// `sudo -S -v` (validate without running a command). Returns Ok(()) iff
    /// sudo accepted the password.
    pub async fn validate_sudo(token: &ElevationToken) -> Result<(), ElevationError> {
        if token.kind() != ElevationKind::Sudo {
            return Err(ElevationError::KindMismatch {
                expected: "Sudo",
                actual: kind_label(token.kind()),
            });
        }

        // Step 1: clear cached creds. Output ignored; failure is non-fatal.
        let _ = Command::new("sudo")
            .arg("-k")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        // Step 2: validate password without running anything else.
        let mut child = match Command::new("sudo")
            .arg("-S")
            .arg("-v")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ElevationError::SudoNotFound);
            }
            Err(e) => return Err(ElevationError::Io(e)),
        };

        if let Some(mut stdin) = child.stdin.take() {
            // Feed password + newline. We hold the secret only as long as the
            // write call lasts; the local `secret_with_nl` Vec is overwritten
            // before drop (Zeroize trait is on Vec<u8>).
            let mut secret_with_nl = token.as_secret_bytes().to_vec();
            secret_with_nl.push(b'\n');
            let write_res = stdin.write_all(&secret_with_nl).await;
            let _ = stdin.shutdown().await;
            // Wipe the local copy as soon as it has been written.
            use zeroize::Zeroize;
            secret_with_nl.zeroize();
            if let Err(e) = write_res {
                let _ = child.kill().await;
                return Err(ElevationError::Io(e));
            }
        }

        let status = child.wait().await?;
        if status.success() {
            Ok(())
        } else {
            Err(ElevationError::InvalidPassword)
        }
    }

    /// Spawn `program` under `sudo -S`, feeding the token's password via stdin
    /// once and immediately closing it. The returned `Child` has `stdin` set to
    /// `Stdio::null()` for the caller's perspective (we already consumed it).
    /// Stdout / stderr are inherited piped — the caller can rebind via the
    /// returned child's handles before calling `wait`.
    pub async fn spawn_sudo(
        token: &ElevationToken,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<tokio::process::Child, ElevationError> {
        if token.kind() != ElevationKind::Sudo {
            return Err(ElevationError::KindMismatch {
                expected: "Sudo",
                actual: kind_label(token.kind()),
            });
        }

        let mut cmd = Command::new("sudo");
        cmd.arg("-S").arg("--").arg(program);
        for a in args {
            cmd.arg(a);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ElevationError::SudoNotFound);
            }
            Err(e) => return Err(ElevationError::Io(e)),
        };

        if let Some(mut stdin) = child.stdin.take() {
            let mut secret_with_nl = token.as_secret_bytes().to_vec();
            secret_with_nl.push(b'\n');
            let write_res = stdin.write_all(&secret_with_nl).await;
            let _ = stdin.shutdown().await;
            use zeroize::Zeroize;
            secret_with_nl.zeroize();
            if let Err(e) = write_res {
                let _ = child.kill().await;
                return Err(ElevationError::Io(e));
            }
        }

        Ok(child)
    }

    /// Validate that the host process is running with Administrator privileges.
    /// Best-effort: spawns `whoami /priv` and looks for `SeShutdownPrivilege`,
    /// which is granted to elevated tokens but not standard ones.
    #[cfg(target_os = "windows")]
    pub async fn validate_admin(token: &ElevationToken) -> Result<(), ElevationError> {
        if token.kind() != ElevationKind::Admin {
            return Err(ElevationError::KindMismatch {
                expected: "Admin",
                actual: kind_label(token.kind()),
            });
        }

        let output = Command::new("whoami")
            .arg("/priv")
            .stdin(Stdio::null())
            .output()
            .await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("SeShutdownPrivilege") {
            Ok(())
        } else {
            Err(ElevationError::Other(
                "process is not running elevated".to_string(),
            ))
        }
    }

    /// Re-launch a command with UAC elevation. Uses the `runas` shell verb,
    /// which the OS evaluates against the interactive user session and prompts
    /// for confirmation. In headless mode (`TF_HEADLESS=1`) we refuse rather
    /// than silently hang waiting for a prompt that no human will see.
    #[cfg(target_os = "windows")]
    pub fn spawn_admin(program: &str, args: &[&str]) -> Result<u32, ElevationError> {
        if std::env::var_os("TF_HEADLESS")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            return Err(ElevationError::HeadlessNotSupported);
        }
        // Compose: powershell -NoProfile -Command Start-Process -Verb runas ...
        // This delegates to the Windows shell which raises the UAC prompt.
        let arg_list = args
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let ps_cmd = if arg_list.is_empty() {
            format!("Start-Process -Verb runas -FilePath '{program}'")
        } else {
            format!("Start-Process -Verb runas -FilePath '{program}' -ArgumentList @({arg_list})")
        };
        let child = std::process::Command::new("powershell")
            .arg("-NoProfile")
            .arg("-Command")
            .arg(&ps_cmd)
            .spawn()
            .map_err(ElevationError::Io)?;
        Ok(child.id())
    }

    /// Stub for non-Windows callers — uniform API surface.
    #[cfg(not(target_os = "windows"))]
    pub fn spawn_admin(_program: &str, _args: &[&str]) -> Result<u32, ElevationError> {
        Err(ElevationError::Other(
            "spawn_admin is only available on Windows".to_string(),
        ))
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn validate_sudo_kind_mismatch_returns_err() {
        let admin = ElevationToken::new_admin();
        let err = ElevationRunner::validate_sudo(&admin).await.unwrap_err();
        match err {
            ElevationError::KindMismatch { expected, actual } => {
                assert_eq!(expected, "Sudo");
                assert_eq!(actual, "Admin");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let lcap = ElevationToken::new_linux_cap();
        let err = ElevationRunner::validate_sudo(&lcap).await.unwrap_err();
        assert!(matches!(err, ElevationError::KindMismatch { .. }));
    }

    #[tokio::test]
    async fn spawn_sudo_kind_mismatch_returns_err() {
        let admin = ElevationToken::new_admin();
        let err = ElevationRunner::spawn_sudo(&admin, "echo", &["hi"], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, ElevationError::KindMismatch { .. }));
    }

    /// Hosts running CI usually do not have an unattended sudo. We keep the test
    /// so it exercises locally; on CI it is harmless because sudo will reject
    /// the wrong password and yield InvalidPassword (or SudoNotFound when the
    /// binary is absent).
    #[tokio::test]
    #[ignore = "requires sudo on host; run manually with `cargo test -- --ignored`"]
    async fn validate_sudo_with_wrong_password_returns_invalid() {
        let token = ElevationToken::new_sudo("definitely-not-the-password".into());
        let err = ElevationRunner::validate_sudo(&token).await.unwrap_err();
        assert!(matches!(
            err,
            ElevationError::InvalidPassword | ElevationError::SudoNotFound
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn spawn_admin_headless_returns_headless_not_supported() {
        std::env::set_var("TF_HEADLESS", "1");
        let err = ElevationRunner::spawn_admin("notepad.exe", &[]).unwrap_err();
        std::env::remove_var("TF_HEADLESS");
        assert!(matches!(err, ElevationError::HeadlessNotSupported));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn spawn_admin_on_non_windows_returns_other() {
        let err = ElevationRunner::spawn_admin("doesntmatter", &[]).unwrap_err();
        assert!(matches!(err, ElevationError::Other(_)));
    }
}
