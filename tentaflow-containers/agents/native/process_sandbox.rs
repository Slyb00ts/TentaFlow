// ============ File: process_sandbox.rs — native filesystem and process confinement ============

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[path = "macos_supervisor.rs"]
mod macos_supervisor;

#[cfg(target_os = "macos")]
pub fn process_birthtime(pid: i32) -> Result<(u64, u64)> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            std::mem::size_of_val(&info) as i32,
        )
    };
    if bytes != std::mem::size_of_val(&info) as i32 {
        return Err(std::io::Error::last_os_error().into());
    }
    if info.pbi_status == 5 {
        return Ok((0, 0));
    }
    Ok((info.pbi_start_tvsec, info.pbi_start_tvusec))
}

pub fn maybe_run_supervisor() -> Option<i32> {
    #[cfg(target_os = "macos")]
    {
        return macos_supervisor::maybe_run();
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub fn is_supervisor_command(argv: &[String]) -> bool {
    cfg!(target_os = "macos")
        && argv
            .get(1)
            .is_some_and(|value| value == "--tentaflow-process-supervisor")
}

pub fn ensure_supervisor_quiescent(root: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos_supervisor::ensure_quiescent(root)?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = root;
    }
    Ok(())
}

pub fn supervisor_root(argv: &[String]) -> Result<Option<PathBuf>> {
    if !is_supervisor_command(argv) {
        return Ok(None);
    }
    let spec: serde_json::Value = serde_json::from_str(
        argv.get(2)
            .context("missing process supervisor configuration")?,
    )?;
    let root = PathBuf::from(spec["root"].as_str().context("missing supervisor root")?);
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let suffix = name.strip_prefix("tfp-").unwrap_or_default();
    if root.parent() != Some(Path::new("/private/tmp"))
        || suffix.len() != 24
        || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("invalid process supervisor root");
    }
    Ok(Some(root))
}

pub fn wait_for_supervisor(root: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match ensure_supervisor_quiescent(root) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// The caller may cancel only after the OS rejected spawn, before any frontend existed.
pub fn cancel_supervisor_launch(argv: &[String]) -> Result<()> {
    let Some(root) = supervisor_root(argv)? else {
        return Ok(());
    };
    let spec: serde_json::Value = serde_json::from_str(&argv[2])?;
    let invocation = Path::new(
        spec["invocation"]
            .as_str()
            .context("missing launch intent")?,
    );
    if invocation.parent() != Some(root.as_path()) {
        bail!("invalid launch intent");
    }
    // remove_dir deliberately refuses a handoff that has already written worker state.
    std::fs::remove_dir(invocation).context("process launch intent is already active")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSandbox {
    workspace: PathBuf,
    private_root: PathBuf,
    read_only: bool,
    read_roots: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
    proxy: Option<std::net::SocketAddr>,
    #[cfg(target_os = "macos")]
    supervisor_root: PathBuf,
}

impl ProcessSandbox {
    pub fn supervisor_root(&self) -> Option<&Path> {
        #[cfg(target_os = "macos")]
        {
            return Some(&self.supervisor_root);
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    pub fn new(
        workspace: &Path,
        private_root: &Path,
        read_only: bool,
        read_roots: &[PathBuf],
        write_roots: &[PathBuf],
    ) -> Result<Self> {
        Self::check_available()?;
        let workspace = canonical_directory(workspace)?;
        let private_root = canonical_directory(private_root)?;
        if workspace.starts_with(&private_root) || private_root.starts_with(&workspace) {
            bail!("sandbox private state and workspace must not overlap");
        }
        let canonical_roots = |roots: &[PathBuf]| -> Result<Vec<PathBuf>> {
            roots.iter().map(|root| canonical_directory(root)).collect()
        };
        let policy = Self {
            workspace,
            private_root,
            read_only,
            read_roots: canonical_roots(read_roots)?,
            write_roots: canonical_roots(write_roots)?,
            proxy: None,
            #[cfg(target_os = "macos")]
            supervisor_root: macos_supervisor::new_root()?,
        };
        #[cfg(target_os = "macos")]
        for granted in std::iter::once(&policy.workspace)
            .chain(std::iter::once(&policy.private_root))
            .chain(policy.read_roots.iter())
            .chain(policy.write_roots.iter())
        {
            if policy.supervisor_root.starts_with(granted) {
                bail!("supervisor state must remain outside every sandbox grant");
            }
        }
        policy.validate()?;
        Ok(policy)
    }

    pub fn with_proxy(mut self, address: std::net::SocketAddr) -> Result<Self> {
        if !address.ip().is_loopback() || address.port() == 0 {
            bail!("sandbox proxy must be a bound loopback endpoint");
        }
        if !cfg!(target_os = "macos") {
            bail!("process sandbox proxy transport is unavailable on this platform");
        }
        self.proxy = Some(address);
        Ok(self)
    }

    pub fn check_available() -> Result<()> {
        #[cfg(target_os = "macos")]
        if Path::new("/usr/bin/sandbox-exec").is_file() {
            return macos_supervisor::check_available();
        }
        #[cfg(target_os = "linux")]
        if Path::new("/usr/bin/bwrap").is_file() {
            return Ok(());
        }
        bail!("process sandbox unavailable: requires macOS sandbox-exec or Linux /usr/bin/bwrap")
    }

    fn validate(&self) -> Result<()> {
        for root in std::iter::once(&self.workspace)
            .chain(std::iter::once(&self.private_root))
            .chain(self.write_roots.iter())
        {
            if canonical_directory(root)? != *root {
                bail!("sandbox root changed: {}", root.display());
            }
            validate_workspace_tree(root)?;
        }
        Ok(())
    }

    pub fn wrap(&self, argv: &[String], cwd: &Path) -> Result<Vec<String>> {
        let command = self.native_command(argv, cwd)?;
        #[cfg(target_os = "macos")]
        {
            return macos_supervisor::wrap(
                command,
                cwd,
                self.supervisor_root().context("missing supervisor root")?,
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(command)
        }
    }

    pub fn ensure_quiescent(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            macos_supervisor::ensure_quiescent(&self.supervisor_root)?;
        }
        Ok(())
    }

    fn native_command(&self, argv: &[String], cwd: &Path) -> Result<Vec<String>> {
        if argv.is_empty() || argv.iter().any(|arg| arg.contains('\0')) {
            bail!("invalid sandbox command");
        }
        self.validate()?;
        let cwd = canonical_directory(cwd)?;
        if !cwd.starts_with(&self.workspace) {
            bail!("sandbox working directory is outside the workspace");
        }
        let mut command = self.platform_command(&cwd)?;
        command.extend_from_slice(argv);
        Ok(command)
    }

    #[cfg(target_os = "macos")]
    fn platform_command(&self, _cwd: &Path) -> Result<Vec<String>> {
        let mut profile = String::from(
            "(version 1)\n(deny default)\n(allow process-exec process-fork)\n\
             (allow process-info* signal (target same-sandbox))\n\
             (allow sysctl-read (sysctl-name-prefix \"hw.\") (sysctl-name-prefix \"machdep.cpu.\") (sysctl-name \"kern.ostype\") (sysctl-name \"kern.osrelease\") (sysctl-name \"kern.osversion\") (sysctl-name \"kern.version\") (sysctl-name \"kern.argmax\") (sysctl-name \"kern.maxfiles\") (sysctl-name \"kern.maxfilesperproc\") (sysctl-name \"kern.osproductversion\") (sysctl-name \"vm.loadavg\"))\n(allow pseudo-tty)\n\
             (allow file-read-data (literal \"/\"))\n(allow file-read-metadata (literal \"/var\") (literal \"/tmp\") (literal \"/etc\"))\n\
             (allow file-read* file-write* (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/ptmx\") (literal \"/dev/tty\"))\n\
             (allow file-read* (literal \"/dev/random\") (literal \"/dev/urandom\"))\n\
             (allow file-ioctl (literal \"/dev/ptmx\") (literal \"/dev/tty\"))\n",
        );
        for root in system_read_roots()
            .iter()
            .chain(self.read_roots.iter())
            .chain(std::iter::once(&self.workspace))
            .chain(std::iter::once(&self.private_root))
            .chain(self.write_roots.iter())
        {
            for ancestor in root.ancestors() {
                profile.push_str(&format!(
                    "(allow file-read-metadata (literal {}))\n",
                    quote_path(ancestor)?
                ));
            }
            profile.push_str(&format!(
                "(allow file-read* (subpath {}))\n",
                quote_path(root)?
            ));
        }
        for root in std::iter::once(&self.private_root)
            .chain(self.write_roots.iter())
            .chain((!self.read_only).then_some(&self.workspace))
        {
            profile.push_str(&format!(
                "(allow file-write* (subpath {}))\n",
                quote_path(root)?
            ));
            profile.push_str(&format!(
                "(deny file-write-unlink (literal {}))\n",
                quote_path(root)?
            ));
        }
        profile.push_str(&format!(
            "(deny file-write* (subpath {}))\n",
            quote_path(&self.workspace.join(".git"))?
        ));
        if let Some(proxy) = self.proxy {
            profile.push_str(&format!(
                "(allow network-outbound (remote tcp \"localhost:{}\"))\n",
                proxy.port()
            ));
        }
        Ok(vec![
            "/usr/bin/sandbox-exec".into(),
            "-p".into(),
            profile,
            "--".into(),
        ])
    }

    #[cfg(target_os = "linux")]
    fn platform_command(&self, cwd: &Path) -> Result<Vec<String>> {
        let mut args: Vec<String> = [
            "/usr/bin/bwrap",
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for root in system_read_roots().iter().chain(self.read_roots.iter()) {
            args.extend([
                "--ro-bind".into(),
                root.display().to_string(),
                root.display().to_string(),
            ]);
        }
        args.extend([
            if self.read_only {
                "--ro-bind"
            } else {
                "--bind"
            }
            .into(),
            self.workspace.display().to_string(),
            self.workspace.display().to_string(),
        ]);
        for root in std::iter::once(&self.private_root).chain(self.write_roots.iter()) {
            args.extend([
                "--bind".into(),
                root.display().to_string(),
                root.display().to_string(),
            ]);
        }
        let git = self.workspace.join(".git");
        if git.exists() {
            args.extend([
                "--ro-bind".into(),
                git.display().to_string(),
                git.display().to_string(),
            ]);
        }
        args.extend(["--chdir".into(), cwd.display().to_string(), "--".into()]);
        Ok(args)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn platform_command(&self, _cwd: &Path) -> Result<Vec<String>> {
        bail!("process sandbox is unsupported on this operating system")
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("sandbox path {}", path.display()))?;
    if !canonical.is_dir() || canonical.parent().is_none() {
        bail!("sandbox root must be a non-root directory");
    }
    Ok(canonical)
}

/// Admission rejects aliases crossing the grant or its protected metadata boundary.
/// Other unsandboxed writers must stay out of admitted directories during a lease.
pub fn validate_workspace_tree(root: &Path) -> Result<()> {
    let started = Instant::now();
    let mut directories = vec![root.to_path_buf()];
    let mut count = 0usize;
    #[cfg(unix)]
    let mut snapshots = Vec::new();
    #[cfg(unix)]
    let mut links: std::collections::HashMap<(u64, u64, bool), (u64, u64)> =
        std::collections::HashMap::new();
    while let Some(directory) = directories.pop() {
        #[cfg(unix)]
        snapshots.push((directory.clone(), std::fs::symlink_metadata(&directory)?));
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            count += 1;
            if count > 1_000_000 || started.elapsed() > Duration::from_secs(30) {
                bail!("sandbox admission scan exceeded its budget");
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            let kind = metadata.file_type();
            if kind.is_dir() {
                directories.push(path.clone());
            } else if kind.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let protected = path.strip_prefix(root)?.components().any(|part| {
                        part.as_os_str()
                            .to_str()
                            .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
                    });
                    let entry = links
                        .entry((metadata.dev(), metadata.ino(), protected))
                        .or_insert((metadata.nlink(), 0));
                    if entry.0 != metadata.nlink() {
                        bail!("sandbox inode changed during admission");
                    }
                    entry.1 += 1;
                }
            } else if !kind.is_symlink() {
                bail!("sandbox rejects special file: {}", path.display());
            }
            #[cfg(unix)]
            snapshots.push((path, metadata));
        }
    }
    #[cfg(unix)]
    {
        if links.values().any(|(total, inside)| total != inside) {
            bail!(
                "sandbox rejects hardlinked file crossing its grant or protected metadata boundary"
            );
        }
        for (path, before) in snapshots {
            if started.elapsed() > Duration::from_secs(30) {
                bail!("sandbox admission scan exceeded its budget");
            }
            let after = std::fs::symlink_metadata(&path)?;
            if metadata_identity(&before) != metadata_identity(&after) {
                bail!("sandbox entry changed during admission: {}", path.display());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn metadata_identity(
    metadata: &std::fs::Metadata,
) -> (u64, u64, u64, u64, u32, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.len(),
        metadata.mode(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(target_os = "macos")]
fn quote_path(path: &Path) -> Result<String> {
    let path = path.to_str().context("sandbox paths must be UTF-8")?;
    if path.chars().any(char::is_control) {
        bail!("sandbox paths must not contain control characters");
    }
    Ok(format!(
        "\"{}\"",
        path.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn system_read_roots() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let paths = [
        "/bin",
        "/sbin",
        "/usr/bin",
        "/usr/sbin",
        "/usr/lib",
        "/usr/libexec",
        "/System/Library",
        "/System/Volumes/Preboot/Cryptexes/OS/usr/lib",
        "/Library/Apple",
        "/private/var/select/sh",
        "/private/etc/ssl",
        "/private/etc/localtime",
    ];
    #[cfg(not(target_os = "macos"))]
    let paths = [
        "/bin",
        "/sbin",
        "/usr/bin",
        "/usr/sbin",
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
        "/etc/ssl",
        "/etc/ld.so.cache",
        "/etc/localtime",
    ];
    let mut roots: Vec<PathBuf> = paths
        .into_iter()
        .filter(|path| Path::new(path).exists())
        .map(PathBuf::from)
        .collect();
    #[cfg(target_os = "macos")]
    for sdk in [
        "/Library/Developer/CommandLineTools",
        "/Applications/Xcode.app/Contents/Developer",
    ] {
        use std::os::unix::fs::MetadataExt;
        if let Ok(metadata) = std::fs::metadata(sdk) {
            if metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0 {
                roots.push(PathBuf::from(sdk));
            }
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "macos")]
    fn supervisor_test_entrypoint() {
        if std::env::var_os("TENTAFLOW_SUPERVISOR_TEST_MODE").is_none() {
            return;
        }
        // Cargo's dylib search paths load the test host, never the sandboxed CLI.
        for name in ["DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
            std::env::remove_var(name);
        }
        unsafe {
            for descriptor in 0..3 {
                assert!(libc::dup2(descriptor + 3, descriptor) >= 0);
                libc::close(descriptor + 3);
            }
        }
        std::process::exit(super::maybe_run_supervisor().expect("supervisor test invocation"));
    }
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn rejected_spawn_releases_only_its_unused_launch_intent() {
        let workspace = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        let policy =
            ProcessSandbox::new(workspace.path(), private.path(), false, &[], &[]).unwrap();
        let mut argv = policy
            .wrap(&["/usr/bin/true".into()], workspace.path())
            .unwrap();
        let root = supervisor_root(&argv).unwrap().unwrap();
        assert!(ensure_supervisor_quiescent(&root).is_err());
        argv[0] = private
            .path()
            .join("absent-executable")
            .display()
            .to_string();
        assert_eq!(
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .spawn()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        cancel_supervisor_launch(&argv).unwrap();
        ensure_supervisor_quiescent(&root).unwrap();

        let argv = policy
            .wrap(&["/usr/bin/true".into()], workspace.path())
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&argv[2]).unwrap();
        let invocation = Path::new(request["invocation"].as_str().unwrap());
        std::fs::write(invocation.join("spec.json"), b"handoff has started").unwrap();
        assert!(cancel_supervisor_launch(&argv).is_err());
        assert!(invocation.join("spec.json").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn admission_counts_only_internal_hardlinks_in_the_same_access_class() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let artifact = project.path().join("artifact");
        std::fs::write(&artifact, b"build output").unwrap();
        std::fs::hard_link(&artifact, project.path().join("artifact-alias")).unwrap();
        validate_workspace_tree(project.path()).unwrap();
        std::fs::hard_link(&artifact, outside.path().join("third-link")).unwrap();
        assert!(validate_workspace_tree(project.path()).is_err());
        std::fs::remove_file(outside.path().join("third-link")).unwrap();
        std::fs::remove_file(project.path().join("artifact-alias")).unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        std::fs::hard_link(&artifact, project.path().join(".git/config")).unwrap();
        assert!(validate_workspace_tree(project.path()).is_err());
        std::fs::remove_file(project.path().join(".git/config")).unwrap();
        std::fs::remove_dir(project.path().join(".git")).unwrap();
        std::fs::create_dir(project.path().join(".GIT")).unwrap();
        std::fs::hard_link(&artifact, project.path().join(".GIT/config")).unwrap();
        assert!(validate_workspace_tree(project.path()).is_err());
        std::fs::remove_file(project.path().join(".GIT/config")).unwrap();
        std::fs::hard_link(&artifact, outside.path().join("third-link")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("third-link"),
            project.path().join("symlink-alias"),
        )
        .unwrap();
        assert!(validate_workspace_tree(project.path()).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn admission_rejects_hardlinks_and_sockets() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "other account").unwrap();
        std::fs::hard_link(outside.path().join("secret"), root.path().join("alias")).unwrap();
        assert!(validate_workspace_tree(root.path())
            .unwrap_err()
            .to_string()
            .contains("hardlinked"));
        std::fs::remove_file(root.path().join("alias")).unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(root.path().join("host.sock")).unwrap();
        assert!(validate_workspace_tree(root.path())
            .unwrap_err()
            .to_string()
            .contains("special file"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn real_process_sandbox_works_on_inherited_pty() {
        assert_inherited_pty(false);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn supervisor_transfers_controlling_pty() {
        assert_inherited_pty(true);
    }

    #[cfg(target_os = "macos")]
    fn assert_inherited_pty(supervised: bool) {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let private = root.path().join("profile");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&private).unwrap();
        let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[]).unwrap();
        let argv = policy
            .native_command(
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "test -t 0 && test -t 1 && printf terminal > pty-result".into(),
                ],
                &workspace,
            )
            .unwrap();
        let argv = if supervised {
            macos_supervisor::wrap(argv, &workspace, &policy.supervisor_root).unwrap()
        } else {
            argv
        };
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        unsafe {
            assert_ne!(libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC), -1);
            assert_ne!(libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC), -1);
        }
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let mut command = std::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &private)
            .current_dir(&workspace)
            .stdin(std::process::Stdio::from(slave.try_clone().unwrap()))
            .stdout(std::process::Stdio::from(slave.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(slave.try_clone().unwrap()));
        let descriptor = slave.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1
                    || (!supervised && libc::ioctl(descriptor, libc::TIOCSCTTY as _, 0) == -1)
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop(command);
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut file = std::fs::File::from(master);
            let mut output = Vec::new();
            let _ = file.read_to_end(&mut output);
            output
        });
        drop(slave);
        let status = child.wait().unwrap();
        let output = reader.join().unwrap();
        assert!(
            status.success(),
            "{status}: {}",
            String::from_utf8_lossy(&output)
        );
        assert_eq!(
            std::fs::read(workspace.join("pty-result")).unwrap(),
            b"terminal"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn supervisor_reaps_double_fork_and_cancellation() {
        for cancel in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("project");
            let private = root.path().join("profile");
            std::fs::create_dir(&workspace).unwrap();
            std::fs::create_dir(&private).unwrap();
            let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[]).unwrap();
            let script = format!("my $c=fork();if($c==0){{setsid();my $g=fork();exit 0 if $g;open(my $f, '>', 'daemon.pid') or die $!;print $f $$;close $f;close STDIN;close STDOUT;close STDERR;sleep 60;exit 0;}}sleep {};exit 0;",if cancel {60}else{1});
            let argv = policy
                .wrap(
                    &[
                        "/usr/bin/perl".into(),
                        "-MPOSIX".into(),
                        "-e".into(),
                        script,
                    ],
                    &workspace,
                )
                .unwrap();
            let mut child = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &private)
                .current_dir(&workspace)
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let daemon_file = workspace.join("daemon.pid");
            while !daemon_file.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(daemon_file.exists(), "daemon never started");
            let pid: i32 = std::fs::read_to_string(daemon_file)
                .unwrap()
                .parse()
                .unwrap();
            if cancel {
                child.kill().unwrap();
            }
            let status = child.wait().unwrap();
            if !cancel {
                assert!(status.success(), "{status}");
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            while policy.ensure_quiescent().is_err() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            policy.ensure_quiescent().unwrap();
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let count = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    std::mem::size_of_val(&info) as i32,
                )
            };
            assert!(
                count == 0 || info.pbi_status == 5,
                "detached descendant survived cleanup"
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn real_process_sandbox_only_reaches_its_proxy() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let private = root.path().join("profile");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&private).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");
        let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[]).unwrap();
        let run = |policy: &ProcessSandbox| {
            let argv = policy
                .native_command(
                    &[
                        "/usr/bin/curl".into(),
                        "--max-time".into(),
                        "2".into(),
                        "--silent".into(),
                        url.clone(),
                    ],
                    &workspace,
                )
                .unwrap();
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .env_clear()
                .env("HOME", &private)
                .current_dir(&workspace)
                .output()
                .unwrap()
        };
        assert!(!run(&policy).status.success());
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((mut connection, _)) => {
                        connection
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = [0; 4096];
                        connection.read(&mut request).unwrap();
                        connection.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
                        return;
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    other => panic!("proxy connection failed: {other:?}"),
                }
            }
        });
        let proxied = policy.with_proxy(address).unwrap();
        let output = run(&proxied);
        assert!(
            output.status.success(),
            "{:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"ok");
        worker.join().unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn real_process_sandbox_confines_descendants_and_read_only_mounts() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let private = root.path().join("profile");
        let outside = root.path().join("other-user");
        for path in [&workspace, &private, &outside] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(workspace.join("input"), "allowed").unwrap();
        std::fs::create_dir(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join(".git/config"), "protected").unwrap();
        std::fs::write(outside.join("secret"), "private").unwrap();
        symlink(outside.join("secret"), workspace.join("alias")).unwrap();
        let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[]).unwrap();
        let run = |policy: &ProcessSandbox, script: &str| {
            let argv = policy
                .native_command(&["/bin/sh".into(), "-c".into(), script.into()], &workspace)
                .unwrap();
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &private)
                .current_dir(&workspace)
                .output()
                .unwrap()
        };
        let output = run(
            &policy,
            "cat input; printf saved > output; printf profile > \"$HOME/state\"",
        );
        assert!(
            output.status.success(),
            "status={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"allowed");
        assert_eq!(std::fs::read(workspace.join("output")).unwrap(), b"saved");
        assert!(!run(&policy, "ln .git/config metadata-alias")
            .status
            .success());
        let output = run(&policy, "sh -c 'cat alias'");
        assert!(!output.status.success());
        assert!(!run(&policy, "printf leaked > alias").status.success());
        assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"private");
        let read_only = ProcessSandbox::new(&workspace, &private, true, &[], &[]).unwrap();
        assert!(!run(&read_only, "printf denied > output").status.success());
        assert!(run(&read_only, "printf yes > \"$HOME/state\"")
            .status
            .success());
        assert!(policy.native_command(&["true".into()], &outside).is_err());
    }
}
