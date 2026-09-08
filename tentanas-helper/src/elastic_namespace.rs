// =============================================================================
// Plik: elastic_namespace.rs
// Opis: Prywatna przestrzeń branchy, zweryfikowana kotwica i publikacja samego FUSE.
// Przykład: run(&paths, Entry::Existing(&anchor), &locks, task, authorize).
// =============================================================================

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Anchor {
    pub(crate) boot_id: String,
    pub(crate) pid: u32,
    pub(crate) start_ticks: u64,
    pub(crate) mount_ns_inode: u64,
    pub(crate) exe_device: u64,
    pub(crate) exe_inode: u64,
    pub(crate) exe_sha256: String,
    pub(crate) union_device: u64,
    pub(crate) union_source: String,
}

pub(crate) struct Paths {
    pub(crate) branch_root: PathBuf,
    pub(crate) union_path: PathBuf,
}

pub(crate) enum Entry<'a> {
    Fresh,
    Existing(&'a Anchor),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicMount {
    pub(crate) device: u64,
    pub(crate) mount_id: u64,
}

pub(crate) struct RunResult<T> {
    pub(crate) value: T,
    pub(crate) public_after: Option<PublicMount>,
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{preflight, run, Worker};

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::elastic::execution::mount_rows;
    use sha2::{Digest, Sha256};
    use std::ffi::CString;
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::{Component, Path};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    const FRAME_LIMIT: usize = 128 * 1024;
    const FUSE_MAGIC: libc::c_long = 0x65735546;
    const HANDSHAKE: Duration = Duration::from_secs(30);

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    enum Message<T> {
        Publish(Anchor),
        Complete(Result<T, String>),
    }

    fn require(condition: bool, message: &str) -> Result<(), String> {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn last_error(operation: &str) -> String {
        format!("{operation}: {}", std::io::Error::last_os_error())
    }

    fn cpath(path: &Path) -> Result<CString, String> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL w ścieżce".into())
    }

    fn boot_id() -> Result<String, String> {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map(|s| s.trim().to_string())
            .map_err(|e| e.to_string())
    }

    fn start_ticks(pid: u32) -> Result<u64, String> {
        let raw =
            std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
        let fields = raw.rsplit_once(") ").ok_or("nieczytelny stat procesu")?.1;
        fields
            .split_whitespace()
            .nth(19)
            .ok_or("brak starttime")?
            .parse()
            .map_err(|_| "nieczytelny starttime".into())
    }

    fn executable(file: &mut File) -> Result<(u64, u64, String), String> {
        let before = file.metadata().map_err(|e| e.to_string())?;
        require(
            before.is_file()
                && before.uid() == 0
                && before.mode() & 0o022 == 0
                && before.len() <= 128 * 1024 * 1024,
            "niebezpieczny plik mergerfs",
        )?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut total = 0usize;
        loop {
            let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            total += count;
            require(total <= 128 * 1024 * 1024, "rosnący plik mergerfs")?;
            hash.update(&buffer[..count]);
        }
        let after = file.metadata().map_err(|e| e.to_string())?;
        require(
            before.dev() == after.dev()
                && before.ino() == after.ino()
                && before.len() == after.len()
                && before.mtime_nsec() == after.mtime_nsec()
                && before.mtime() == after.mtime()
                && before.ctime() == after.ctime()
                && before.ctime_nsec() == after.ctime_nsec(),
            "zmieniony plik mergerfs",
        )?;
        Ok((
            before.dev(),
            before.ino(),
            hash.finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        ))
    }

    fn validate_anchor(anchor: &Anchor) -> Result<OwnedFd, String> {
        require(
            anchor.pid > 1
                && anchor.start_ticks > 0
                && anchor.mount_ns_inode > 0
                && anchor.union_device > 0
                && anchor.exe_sha256.len() == 64
                && anchor.exe_sha256.bytes().all(|b| b.is_ascii_hexdigit())
                && !anchor.union_source.is_empty(),
            "niepełna kotwica",
        )?;
        require(
            boot_id()? == anchor.boot_id && start_ticks(anchor.pid)? == anchor.start_ticks,
            "obcy boot/PID kotwicy",
        )?;
        let file = File::open(format!("/proc/{}/ns/mnt", anchor.pid)).map_err(|e| e.to_string())?;
        require(
            file.metadata().map_err(|e| e.to_string())?.ino() == anchor.mount_ns_inode,
            "obca namespace kotwicy",
        )?;
        require(
            std::fs::metadata("/proc/self/ns/mnt")
                .map_err(|e| e.to_string())?
                .ino()
                != anchor.mount_ns_inode,
            "kotwica w hostowej namespace",
        )?;
        let mut exe = File::open(format!("/proc/{}/exe", anchor.pid)).map_err(|e| e.to_string())?;
        let measured = executable(&mut exe)?;
        require(
            measured
                == (
                    anchor.exe_device,
                    anchor.exe_inode,
                    anchor.exe_sha256.clone(),
                ),
            "obce exe kotwicy",
        )?;
        require(
            start_ticks(anchor.pid)? == anchor.start_ticks
                && std::fs::metadata(format!("/proc/{}/ns/mnt", anchor.pid))
                    .map_err(|e| e.to_string())?
                    .ino()
                    == anchor.mount_ns_inode,
            "podmieniona kotwica podczas odczytu",
        )?;
        Ok(file.into())
    }

    fn filesystem(fd: RawFd) -> Result<(libc::c_long, u64), String> {
        let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
        let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstatfs(fd, fs.as_mut_ptr()) } != 0
            || unsafe { libc::fstat(fd, info.as_mut_ptr()) } != 0
        {
            return Err(last_error("fstatfs/fstat"));
        }
        Ok(unsafe { (fs.assume_init().f_type, info.assume_init().st_dev) })
    }

    fn public_mount(path: &Path, anchor: Option<&Anchor>) -> Result<Option<PublicMount>, String> {
        let rows = mount_rows()?;
        let found: Vec<_> = rows
            .iter()
            .filter(|row| Path::new(&row.path) == path)
            .collect();
        if found.is_empty() {
            if let Err(error) = std::fs::symlink_metadata(path) {
                if error.kind() == std::io::ErrorKind::NotFound {
                    secure_directory(path.parent().ok_or("brak parenta unii")?, false)?;
                    return Ok(None);
                }
                return Err(error.to_string());
            }
            secure_directory(path, true)?;
            require(
                std::fs::read_dir(path)
                    .map_err(|e| e.to_string())?
                    .next()
                    .is_none(),
                "niepusty publiczny mountpoint",
            )?;
            return Ok(None);
        }
        let anchor = anchor.ok_or("nieoczekiwany publiczny mount")?;
        require(
            found.len() == 1
                && found[0].filesystem == "fuse.mergerfs"
                && found[0].root == "/"
                && found[0].source == anchor.union_source
                && found[0].id > 0
                && !found[0].mount_options.is_empty()
                && !found[0].super_options.is_empty(),
            "obcy publiczny mount",
        )?;
        let fd = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| e.to_string())?;
        require(
            filesystem(fd.as_raw_fd())? == (FUSE_MAGIC, anchor.union_device),
            "obcy publiczny FUSE",
        )?;
        let number = format!(
            "{}:{}",
            libc::major(anchor.union_device),
            libc::minor(anchor.union_device)
        );
        require(
            found[0].major_minor == number,
            "obcy numer publicznego urządzenia",
        )?;
        Ok(Some(PublicMount {
            device: anchor.union_device,
            mount_id: found[0].id,
        }))
    }

    fn secure_directory(path: &Path, private: bool) -> Result<(), String> {
        require(path.is_absolute(), "względna ścieżka namespace")?;
        let mut current = PathBuf::from("/");
        for part in path.components().skip(1) {
            require(
                matches!(part, Component::Normal(_)),
                "niekanoniczna ścieżka namespace",
            )?;
            current.push(part);
            let metadata = std::fs::symlink_metadata(&current).map_err(|e| e.to_string())?;
            require(
                metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == 0
                    && metadata.mode() & 0o022 == 0,
                "niebezpieczny katalog namespace",
            )?;
            if current == path && private {
                require(
                    metadata.mode() & 0o777 == 0o700,
                    "mountpoint nie jest root0700",
                )?;
            }
        }
        Ok(())
    }

    fn single_thread() -> Result<(), String> {
        require(unsafe { libc::geteuid() } == 0, "namespace wymaga root")?;
        let count = std::fs::read_dir("/proc/self/task")
            .map_err(|e| e.to_string())?
            .count();
        require(count == 1, "fork namespace wymaga jednowątkowego helpera")
    }

    fn socket_pair() -> Result<(OwnedFd, OwnedFd), String> {
        let mut descriptors = [-1; 2];
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                descriptors.as_mut_ptr(),
            )
        } != 0
        {
            return Err(last_error("socketpair"));
        }
        let pair = unsafe {
            (
                OwnedFd::from_raw_fd(descriptors[0]),
                OwnedFd::from_raw_fd(descriptors[1]),
            )
        };
        let timeout = libc::timeval {
            tv_sec: HANDSHAKE.as_secs() as _,
            tv_usec: 0,
        };
        for socket in [&pair.0, &pair.1] {
            if unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDTIMEO,
                    (&timeout as *const libc::timeval).cast(),
                    std::mem::size_of_val(&timeout) as _,
                )
            } != 0
            {
                return Err(last_error("timeout wysyłki namespace"));
            }
        }
        Ok(pair)
    }

    fn wait_readable(fd: RawFd, timeout: Duration) -> Result<bool, String> {
        let mut event = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe {
            libc::poll(
                &mut event,
                1,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if result < 0 {
            return Err(last_error("poll namespace"));
        }
        Ok(result > 0)
    }

    fn send_frame<T: Serialize>(socket: RawFd, value: &T, fd: Option<RawFd>) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        require(bytes.len() <= FRAME_LIMIT, "za duży komunikat namespace")?;
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr().cast_mut().cast(),
            iov_len: bytes.len(),
        };
        let mut control = [0usize; 32];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        if let Some(fd) = fd {
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen =
                unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&message);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
                std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd);
            }
        }
        let count = unsafe { libc::sendmsg(socket, &message, libc::MSG_NOSIGNAL) };
        require(
            count == bytes.len() as isize,
            &format!("sendmsg namespace: {}", std::io::Error::last_os_error()),
        )
    }

    fn receive_frame<T: DeserializeOwned>(socket: RawFd) -> Result<(T, Vec<OwnedFd>), String> {
        let mut bytes = vec![0u8; FRAME_LIMIT];
        let mut control = [0usize; 32];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control);
        let count = unsafe { libc::recvmsg(socket, &mut message, libc::MSG_CMSG_CLOEXEC) };
        if count < 0 {
            return Err(last_error("recvmsg namespace"));
        }
        let mut descriptors = Vec::new();
        let mut valid = true;
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&message);
            while !header.is_null() {
                if (*header).cmsg_level != libc::SOL_SOCKET
                    || (*header).cmsg_type != libc::SCM_RIGHTS
                {
                    valid = false;
                } else {
                    let length = (*header)
                        .cmsg_len
                        .saturating_sub(libc::CMSG_LEN(0) as usize);
                    valid &= length.is_multiple_of(std::mem::size_of::<RawFd>());
                    for index in 0..length / std::mem::size_of::<RawFd>() {
                        let fd = std::ptr::read_unaligned(
                            libc::CMSG_DATA(header).cast::<RawFd>().add(index),
                        );
                        descriptors.push(OwnedFd::from_raw_fd(fd));
                    }
                }
                header = libc::CMSG_NXTHDR(&message, header);
            }
        }
        require(
            count > 0 && valid && message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) == 0,
            "niepełny lub obcy komunikat namespace",
        )?;
        bytes.truncate(count as usize);
        let value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok((value, descriptors))
    }

    fn private_overlay(branch_root: &Path) -> Result<(), String> {
        if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
            return Err(last_error("unshare"));
        }
        if unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(last_error("prywatna propagacja"));
        }
        let path = cpath(branch_root)?;
        if unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                path.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"size=4194304,mode=0711".as_ptr().cast(),
            )
        } != 0
        {
            return Err(last_error("prywatny korzeń branchy"));
        }
        Ok(())
    }

    fn clone_mount(path: &Path) -> Result<OwnedFd, String> {
        let path = cpath(path)?;
        let fd = unsafe {
            libc::syscall(
                libc::SYS_open_tree,
                libc::AT_FDCWD,
                path.as_ptr(),
                1u32 | libc::O_CLOEXEC as u32,
            )
        };
        if fd < 0 {
            return Err(last_error("open_tree FUSE"));
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        require(
            filesystem(owned.as_raw_fd())?.0 == FUSE_MAGIC,
            "klon nie jest FUSE",
        )?;
        Ok(owned)
    }

    fn attach_mount(fd: RawFd, path: &Path) -> Result<(), String> {
        let path = cpath(path)?;
        if unsafe {
            libc::syscall(
                libc::SYS_move_mount,
                fd,
                c"".as_ptr(),
                libc::AT_FDCWD,
                path.as_ptr(),
                4u32,
            )
        } != 0
        {
            return Err(last_error("move_mount FUSE"));
        }
        Ok(())
    }

    fn internal_mount(path: &Path, anchor: &Anchor) -> Result<(), String> {
        internal_mount_state(path, anchor).map(|_| ())
    }

    struct InternalMountState {
        flags: libc::c_ulong,
        readonly: bool,
        mount_readonly: bool,
        mount_id: u64,
    }

    fn remount_flags(mount_options: &[String], super_options: &[String]) -> libc::c_ulong {
        let mut flags = 0;
        for option in mount_options.iter().chain(super_options) {
            flags |= match option.as_str() {
                "nosuid" => libc::MS_NOSUID,
                "nodev" => libc::MS_NODEV,
                "noexec" => libc::MS_NOEXEC,
                "noatime" => libc::MS_NOATIME,
                "nodiratime" => libc::MS_NODIRATIME,
                "relatime" => libc::MS_RELATIME,
                "strictatime" => libc::MS_STRICTATIME,
                "lazytime" => libc::MS_LAZYTIME,
                "nosymfollow" => libc::MS_NOSYMFOLLOW,
                "iversion" => libc::MS_I_VERSION,
                "mand" => libc::MS_MANDLOCK,
                "sync" => libc::MS_SYNCHRONOUS,
                "dirsync" => libc::MS_DIRSYNC,
                "rw" | "ro" | "suid" | "dev" | "exec" | "atime" => 0,
                _ => 0,
            };
        }
        flags
    }

    fn readonly_option(options: &[String], context: &str) -> Result<bool, String> {
        let states: Vec<_> = options
            .iter()
            .filter_map(|option| match option.as_str() {
                "ro" => Some(true),
                "rw" => Some(false),
                _ => None,
            })
            .collect();
        require(
            states.len() == 1,
            context,
        )?;
        Ok(states[0])
    }

    fn internal_mount_state(path: &Path, anchor: &Anchor) -> Result<InternalMountState, String> {
        let rows = mount_rows()?;
        let matching: Vec<_> = rows
            .iter()
            .filter(|row| Path::new(&row.path) == path)
            .collect();
        require(
            matching.len() == 1
                && matching[0].filesystem == "fuse.mergerfs"
                && matching[0].root == "/"
                && matching[0].source == anchor.union_source,
            "obca wewnętrzna unia kotwicy",
        )?;
        let row = matching[0];
        let readonly = readonly_option(
            &row.super_options,
            "nieznany lub sprzeczny stan superblocka unii",
        )?;
        let local_readonly = readonly_option(
            &row.mount_options,
            "nieznany stan lokalnego mounta unii",
        )?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| e.to_string())?;
        require(
            filesystem(file.as_raw_fd())? == (FUSE_MAGIC, anchor.union_device),
            "obce urządzenie wewnętrznej unii",
        )?;
        let number = format!(
            "{}:{}",
            libc::major(anchor.union_device),
            libc::minor(anchor.union_device)
        );
        require(
            row.id > 0 && row.major_minor == number,
            "obca tożsamość wewnętrznej unii",
        )?;
        Ok(InternalMountState {
            flags: remount_flags(&row.mount_options, &row.super_options),
            readonly,
            mount_readonly: local_readonly,
            mount_id: row.id,
        })
    }

    pub(crate) struct Worker<'a> {
        paths: &'a Paths,
        socket: RawFd,
        anchor: Option<Anchor>,
        public: Option<PublicMount>,
        daemon: Option<Child>,
    }

    fn daemon_command(program: &Path, args: &[String]) -> Command {
        let mut command = Command::new(program);
        command
            .arg("-f")
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                // Deskryptor błędu exec pozostaje dostępny aż do udanego execve.
                if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    }

    impl Worker<'_> {
        pub(crate) fn public_mount(&self) -> Option<&PublicMount> {
            self.public.as_ref()
        }

        pub(crate) fn start_mergerfs(
            &mut self,
            program: &Path,
            args: &[String],
            log: &File,
        ) -> Result<Anchor, String> {
            require(
                self.anchor.is_none() && self.daemon.is_none(),
                "kotwica już istnieje",
            )?;
            let mut program_file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(program)
                .map_err(|e| e.to_string())?;
            let expected_exe = executable(&mut program_file)?;
            self.daemon = Some(
                daemon_command(program, args)
                    .spawn()
                    .map_err(|e| e.to_string())?,
            );
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let daemon = self.daemon.as_mut().ok_or("brak procesu mergerfs")?;
                require(
                    daemon.try_wait().map_err(|e| e.to_string())?.is_none(),
                    "mergerfs zakończył start",
                )?;
                let rows = mount_rows()?;
                let found: Vec<_> = rows
                    .iter()
                    .filter(|row| Path::new(&row.path) == self.paths.union_path)
                    .collect();
                if !found.is_empty() {
                    require(
                        found.len() == 1
                            && found[0].filesystem == "fuse.mergerfs"
                            && found[0].root == "/"
                            && !found[0].source.is_empty(),
                        "obcy mount przy starcie mergerfs",
                    )?;
                    let pid = daemon.id();
                    let ticks = start_ticks(pid)?;
                    let mut exe =
                        File::open(format!("/proc/{pid}/exe")).map_err(|e| e.to_string())?;
                    require(
                        executable(&mut exe)? == expected_exe,
                        "niezgodny uruchomiony mergerfs",
                    )?;
                    let ns = std::fs::metadata(format!("/proc/{pid}/ns/mnt"))
                        .map_err(|e| e.to_string())?
                        .ino();
                    require(
                        ns == std::fs::metadata("/proc/self/ns/mnt")
                            .map_err(|e| e.to_string())?
                            .ino()
                            && start_ticks(pid)? == ticks,
                        "podmieniony proces mergerfs",
                    )?;
                    let union = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
                        .open(&self.paths.union_path)
                        .map_err(|e| e.to_string())?;
                    let (kind, device) = filesystem(union.as_raw_fd())?;
                    require(kind == FUSE_MAGIC, "unia nie jest FUSE")?;
                    let anchor = Anchor {
                        boot_id: boot_id()?,
                        pid,
                        start_ticks: ticks,
                        mount_ns_inode: ns,
                        exe_device: expected_exe.0,
                        exe_inode: expected_exe.1,
                        exe_sha256: expected_exe.2,
                        union_device: device,
                        union_source: found[0].source.clone(),
                    };
                    let mut record = log;
                    serde_json::to_writer(&mut record, &anchor).map_err(|e| e.to_string())?;
                    record.write_all(b"\n").map_err(|e| e.to_string())?;
                    record.sync_all().map_err(|e| e.to_string())?;
                    self.anchor = Some(anchor.clone());
                    return Ok(anchor);
                }
                require(Instant::now() < deadline, "timeout startu mergerfs")?;
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        pub(crate) fn publish(&mut self, anchor: &Anchor) -> Result<PublicMount, String> {
            require(
                self.anchor.as_ref() == Some(anchor),
                "publikacja obcej kotwicy",
            )?;
            internal_mount(&self.paths.union_path, anchor)?;
            let fd = clone_mount(&self.paths.union_path)?;
            send_frame(
                self.socket,
                &Message::<()>::Publish(anchor.clone()),
                Some(fd.as_raw_fd()),
            )?;
            require(
                wait_readable(self.socket, HANDSHAKE)?,
                "timeout ACK publikacji",
            )?;
            let (response, descriptors): (Result<PublicMount, String>, _) =
                receive_frame(self.socket)?;
            require(descriptors.is_empty(), "deskryptor w ACK publikacji")?;
            let public = response?;
            require(
                public.device == anchor.union_device && public.mount_id > 0,
                "obcy ACK publikacji",
            )?;
            self.public = Some(public.clone());
            Ok(public)
        }

        pub(crate) fn union_readonly(&self) -> Result<bool, String> {
            let anchor = self.anchor.as_ref().ok_or("brak kotwicy unii")?;
            Ok(internal_mount_state(&self.paths.union_path, anchor)?.readonly)
        }

        pub(crate) fn set_union_readonly(&mut self, readonly: bool) -> Result<(), String> {
            let anchor = self.anchor.as_ref().ok_or("brak kotwicy unii")?;
            let state = internal_mount_state(&self.paths.union_path, anchor)?;
            if state.readonly == readonly && state.mount_readonly == readonly {
                return Ok(());
            }
            let path = cpath(&self.paths.union_path)?;
            let mut flags = state.flags | libc::MS_REMOUNT;
            if readonly {
                flags |= libc::MS_RDONLY;
            }
            if unsafe {
                libc::mount(
                    std::ptr::null(),
                    path.as_ptr(),
                    std::ptr::null(),
                    flags,
                    std::ptr::null(),
                )
            } != 0
            {
                return Err(last_error("remount globalnego FUSE"));
            }
            let after = internal_mount_state(&self.paths.union_path, anchor)?;
            require(
                after.readonly == readonly
                    && after.mount_readonly == readonly
                    && after.mount_id == state.mount_id
                    && after.flags == state.flags,
                "remount nie zmienił stanu superblocka unii",
            )
        }
    }

    fn child_descriptors(keep: &[RawFd]) -> Result<(), String> {
        let descriptors: Vec<RawFd> = std::fs::read_dir("/proc/self/fd")
            .map_err(|e| e.to_string())?
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        for fd in descriptors {
            if fd > 2 && !keep.contains(&fd) {
                unsafe {
                    libc::close(fd);
                }
            }
        }
        std::env::set_current_dir("/").map_err(|e| e.to_string())
    }

    fn wait_child(pid: libc::pid_t, timeout: Duration) -> Result<Option<i32>, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut status = 0;
            let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if result == pid {
                return Ok(Some(status));
            }
            if result < 0 {
                return Err(last_error("waitpid namespace"));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminate_child(pid: libc::pid_t) {
        if !matches!(wait_child(pid, Duration::ZERO), Ok(None)) {
            return;
        }
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        if matches!(wait_child(pid, Duration::from_secs(2)), Ok(None)) {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = wait_child(pid, Duration::from_secs(2));
        }
    }

    fn execute<T: Serialize + DeserializeOwned>(
        paths: &Paths,
        entry: Entry<'_>,
        locks: &[RawFd],
        before: Option<PublicMount>,
        task: impl FnOnce(&mut Worker<'_>) -> Result<T, String>,
        mut authorize: impl FnMut(&Anchor) -> Result<(), String>,
    ) -> Result<(T, Option<Anchor>), String> {
        single_thread()?;
        secure_directory(&paths.branch_root, true)?;
        let initial = match entry {
            Entry::Fresh => None,
            Entry::Existing(anchor) => Some(anchor.clone()),
        };
        let namespace = initial.as_ref().map(validate_anchor).transpose()?;
        let (parent, child) = socket_pair()?;
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(last_error("fork namespace"));
        }
        if pid == 0 {
            let result = (|| {
                let mut keep = locks.to_vec();
                keep.push(child.as_raw_fd());
                if let Some(fd) = namespace.as_ref() {
                    keep.push(fd.as_raw_fd());
                }
                child_descriptors(&keep)?;
                if let Some(fd) = namespace.as_ref() {
                    if unsafe { libc::setns(fd.as_raw_fd(), libc::CLONE_NEWNS) } != 0 {
                        return Err(last_error("setns kotwicy"));
                    }
                    let anchor = initial.as_ref().ok_or("brak kotwicy setns")?;
                    require(
                        std::fs::metadata("/proc/self/ns/mnt")
                            .map_err(|e| e.to_string())?
                            .ino()
                            == anchor.mount_ns_inode,
                        "setns innej kotwicy",
                    )?;
                    internal_mount(&paths.union_path, anchor)?;
                } else {
                    private_overlay(&paths.branch_root)?;
                }
                let mut worker = Worker {
                    paths,
                    socket: child.as_raw_fd(),
                    anchor: initial.clone(),
                    public: before,
                    daemon: None,
                };
                task(&mut worker)
            })();
            let sent = send_frame(child.as_raw_fd(), &Message::Complete(result), None).is_ok();
            unsafe {
                libc::_exit(if sent { 0 } else { 1 });
            }
        }
        drop(child);
        let mut reaped = false;
        let result = (|| {
            let mut anchor = initial;
            loop {
                if !wait_readable(parent.as_raw_fd(), Duration::from_secs(1))? {
                    if wait_child(pid, Duration::ZERO)?.is_some() {
                        reaped = true;
                        return Err("worker zakończył się bez wyniku".into());
                    }
                    continue;
                }
                let (message, descriptors): (Message<T>, _) = receive_frame(parent.as_raw_fd())?;
                match message {
                    Message::Complete(value) => {
                        require(descriptors.is_empty(), "deskryptor w wyniku workera")?;
                        let status = wait_child(pid, Duration::from_secs(2))?
                            .ok_or("worker nie zakończył się po wyniku")?;
                        reaped = true;
                        require(
                            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                            "błędne wyjście workera",
                        )?;
                        return Ok((value?, anchor));
                    }
                    Message::Publish(candidate) => {
                        let published: Result<PublicMount, String> = (|| {
                            require(descriptors.len() == 1, "publikacja wymaga jednego FD")?;
                            let _namespace = validate_anchor(&candidate)?;
                            require(
                                filesystem(descriptors[0].as_raw_fd())?
                                    == (FUSE_MAGIC, candidate.union_device),
                                "obcy deskryptor publikacji",
                            )?;
                            authorize(&candidate)?;
                            if public_mount(&paths.union_path, Some(&candidate))?.is_none() {
                                secure_directory(&paths.union_path, true)?;
                                attach_mount(descriptors[0].as_raw_fd(), &paths.union_path)?;
                            }
                            let public = public_mount(&paths.union_path, Some(&candidate))?
                                .ok_or("brak opublikowanej unii")?;
                            require(
                                public_mount(&paths.union_path, Some(&candidate))?.as_ref()
                                    == Some(&public),
                                "zmiana publikacji przed ACK",
                            )?;
                            Ok(public)
                        })();
                        if published.is_ok() {
                            anchor = Some(candidate);
                        }
                        send_frame(parent.as_raw_fd(), &published, None)?;
                    }
                }
            }
        })();
        if result.is_err() && !reaped {
            terminate_child(pid);
        }
        result
    }

    pub(crate) fn run<T: Serialize + DeserializeOwned>(
        paths: &Paths,
        entry: Entry<'_>,
        locks: &[RawFd],
        task: impl FnOnce(&mut Worker<'_>) -> Result<T, String>,
        authorize: impl FnMut(&Anchor) -> Result<(), String>,
    ) -> Result<RunResult<T>, String> {
        let anchor = match &entry {
            Entry::Fresh => None,
            Entry::Existing(anchor) => Some(*anchor),
        };
        let before = public_mount(&paths.union_path, anchor)?;
        let (value, anchor) = execute(paths, entry, locks, before.clone(), task, authorize)?;
        let after = public_mount(&paths.union_path, anchor.as_ref())?;
        require(
            before.is_none() || before == after,
            "zmieniona istniejąca publikacja podczas operacji",
        )?;
        Ok(RunResult {
            value,
            public_after: after,
        })
    }

    pub(crate) fn preflight(paths: &Paths, mergerfs: &Path) -> Result<(), String> {
        let scratch = Paths {
            branch_root: paths.branch_root.clone(),
            union_path: paths.branch_root.join(".namespace-preflight-union"),
        };
        execute(
            &scratch,
            Entry::Fresh,
            &[],
            None,
            |worker| {
                let data = scratch.branch_root.join(".namespace-preflight-data");
                let target = scratch.branch_root.join(".namespace-preflight-target");
                for path in [&data, &target, &scratch.union_path] {
                    std::fs::DirBuilder::new()
                        .mode(0o755)
                        .create(path)
                        .map_err(|e| e.to_string())?;
                }
                let log = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(scratch.branch_root.join(".namespace-preflight.log"))
                    .map_err(|e| e.to_string())?;
                let result = (|| {
                    let args = vec![
                        "-o".into(),
                        "allow_other,cache.files=off".into(),
                        data.to_string_lossy().into_owned(),
                        scratch.union_path.to_string_lossy().into_owned(),
                    ];
                    let anchor = worker.start_mergerfs(mergerfs, &args, &log)?;
                    let cloned = clone_mount(&scratch.union_path)?;
                    attach_mount(cloned.as_raw_fd(), &target)?;
                    drop(cloned);
                    internal_mount(&target, &anchor)?;
                    for path in [&target, &scratch.union_path] {
                        if unsafe { libc::umount2(cpath(path)?.as_ptr(), 0) } != 0 {
                            return Err(last_error("umount preflight"));
                        }
                    }
                    Ok(())
                })();
                if let Some(daemon) = worker.daemon.as_mut() {
                    if daemon.try_wait().map_err(|e| e.to_string())?.is_none() {
                        terminate_child(daemon.id() as libc::pid_t);
                    }
                }
                result
            },
            |_| Err("preflight nie publikuje na hoście".into()),
        )
        .map(|_| ())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicBool, Ordering};

        #[test]
        fn superblock_state_requires_exactly_one_global_flag() {
            assert!(!readonly_option(&["rw".into(), "relatime".into()], "state").unwrap());
            assert!(readonly_option(&["ro".into(), "nosuid".into()], "state").unwrap());
            assert!(readonly_option(&["ro".into(), "rw".into()], "state").is_err());
            assert!(readonly_option(&["relatime".into()], "state").is_err());
        }

        #[test]
        fn remount_flags_preserve_security_and_atime_options() {
            let flags = remount_flags(
                &[
                    "rw".into(),
                    "nosuid".into(),
                    "nodev".into(),
                    "noexec".into(),
                ],
                &["relatime".into(), "sync".into()],
            );
            assert_ne!(flags & libc::MS_NOSUID, 0);
            assert_ne!(flags & libc::MS_NODEV, 0);
            assert_ne!(flags & libc::MS_NOEXEC, 0);
            assert_ne!(flags & libc::MS_RELATIME, 0);
            assert_ne!(flags & libc::MS_SYNCHRONOUS, 0);
            assert_eq!(flags & libc::MS_RDONLY, 0);
        }

        #[test]
        fn worker_refuses_set_without_anchor_before_mount_access() {
            let paths = Paths {
                branch_root: PathBuf::from("/var/empty"),
                union_path: PathBuf::from("/var/empty/union"),
            };
            let worker = Worker {
                paths: &paths,
                socket: -1,
                anchor: None,
                public: None,
                daemon: None,
            };
            assert!(worker.union_readonly().is_err());
            let mut worker = worker;
            assert!(worker.set_union_readonly(true).is_err());
            assert!(worker.set_union_readonly(false).is_err());
        }

        #[test]
        fn packet_transfers_actual_fd_with_close_on_exec() {
            let (sender, receiver) = socket_pair().unwrap();
            let file = File::open("/dev/null").unwrap();
            send_frame(sender.as_raw_fd(), &vec![1u32, 2], Some(file.as_raw_fd())).unwrap();
            let (value, fds): (Vec<u32>, _) = receive_frame(receiver.as_raw_fd()).unwrap();
            assert_eq!(value, [1, 2]);
            assert_eq!(fds.len(), 1);
            assert_eq!(
                filesystem(fds[0].as_raw_fd()).unwrap(),
                filesystem(file.as_raw_fd()).unwrap()
            );
            assert_ne!(
                unsafe { libc::fcntl(fds[0].as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
        }

        #[test]
        fn rejected_json_closes_received_pipe_writer() {
            let (sender, receiver) = socket_pair().unwrap();
            let mut pipe = [-1; 2];
            assert_eq!(
                unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) },
                0
            );
            let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
            let write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
            send_frame(
                sender.as_raw_fd(),
                &"nie jest liczbą",
                Some(write.as_raw_fd()),
            )
            .unwrap();
            drop(write);
            assert!(receive_frame::<u64>(receiver.as_raw_fd()).is_err());
            let mut byte = 0u8;
            assert_eq!(
                unsafe { libc::read(read.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) },
                0
            );
        }

        #[test]
        fn frame_limits_reject_before_send_and_after_truncation() {
            let (sender, receiver) = socket_pair().unwrap();
            assert!(send_frame(sender.as_raw_fd(), &"x".repeat(FRAME_LIMIT), None).is_err());
            assert!(!wait_readable(receiver.as_raw_fd(), Duration::ZERO).unwrap());
            let bytes = vec![b'x'; FRAME_LIMIT + 1];
            assert_eq!(
                unsafe {
                    libc::send(
                        sender.as_raw_fd(),
                        bytes.as_ptr().cast(),
                        bytes.len(),
                        libc::MSG_NOSIGNAL,
                    )
                },
                bytes.len() as isize
            );
            assert!(receive_frame::<serde_json::Value>(receiver.as_raw_fd()).is_err());
        }

        #[test]
        fn anchor_rejects_host_namespace_before_executable_adoption() {
            let pid = std::process::id();
            let anchor = Anchor {
                boot_id: boot_id().unwrap(),
                pid,
                start_ticks: start_ticks(pid).unwrap(),
                mount_ns_inode: std::fs::metadata("/proc/self/ns/mnt").unwrap().ino(),
                exe_device: 1,
                exe_inode: 1,
                exe_sha256: "a".repeat(64),
                union_device: 1,
                union_source: "fixture".into(),
            };
            assert!(validate_anchor(&anchor)
                .unwrap_err()
                .contains("hostowej namespace"));
            let mut value = serde_json::to_value(&anchor).unwrap();
            value["extra"] = serde_json::json!(true);
            assert!(serde_json::from_value::<Anchor>(value).is_err());
        }

        #[test]
        fn execute_refuses_test_process_without_running_callbacks() {
            let called = AtomicBool::new(false);
            let paths = Paths {
                branch_root: PathBuf::from("/not-created"),
                union_path: PathBuf::from("/not-created-union"),
            };
            let result = execute(
                &paths,
                Entry::Fresh,
                &[],
                None,
                |_| {
                    called.store(true, Ordering::SeqCst);
                    Ok(())
                },
                |_| {
                    called.store(true, Ordering::SeqCst);
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert!(!called.load(Ordering::SeqCst));
        }

        #[test]
        fn daemon_exec_closes_high_fd_and_preserves_file_size_limit() {
            let _isolation = crate::elastic::execution::tests::FORK_REOPEN
                .lock()
                .unwrap();
            let file = File::open("/dev/null").unwrap();
            let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 128) };
            assert!(fd >= 128);
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            let inherited = Command::new("/bin/sh")
                .args(["-c", "ulimit -f"])
                .output()
                .unwrap();
            assert!(inherited.status.success());
            let expected = String::from_utf8(inherited.stdout).unwrap();
            let args = vec![
                "-c".into(),
                "test ! -e /proc/self/fd/$1 && test \"$(ulimit -f)\" = \"$2\"".into(),
                "probe".into(),
                owned.as_raw_fd().to_string(),
                expected.trim().into(),
            ];
            assert!(daemon_command(Path::new("/bin/sh"), &args)
                .status()
                .unwrap()
                .success());
            assert!(filesystem(owned.as_raw_fd()).is_ok());
        }

        #[test]
        fn bounded_join_reaps_real_child_and_cleanup_does_not_reap_twice() {
            let _isolation = crate::elastic::execution::tests::FORK_REOPEN
                .lock()
                .unwrap();
            let mut child = Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .spawn()
                .unwrap();
            let pid = child.id() as libc::pid_t;
            let status = wait_child(pid, Duration::from_secs(2)).unwrap().unwrap();
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 7);
            terminate_child(pid);
            assert!(wait_child(pid, Duration::ZERO).is_err());
            assert_eq!(child.wait().unwrap_err().raw_os_error(), Some(libc::ECHILD));
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) struct Worker<'a>(std::marker::PhantomData<&'a Paths>);

#[cfg(not(target_os = "linux"))]
impl Worker<'_> {
    pub(crate) fn public_mount(&self) -> Option<&PublicMount> {
        None
    }
    pub(crate) fn start_mergerfs(
        &mut self,
        _: &std::path::Path,
        _: &[String],
        _: &std::fs::File,
    ) -> Result<Anchor, String> {
        Err("prywatna namespace wymaga Linux".into())
    }
    pub(crate) fn publish(&mut self, _: &Anchor) -> Result<PublicMount, String> {
        Err("prywatna namespace wymaga Linux".into())
    }
    pub(crate) fn union_readonly(&self) -> Result<bool, String> {
        Err("prywatna namespace wymaga Linux".into())
    }
    pub(crate) fn set_union_readonly(&mut self, _: bool) -> Result<(), String> {
        Err("prywatna namespace wymaga Linux".into())
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn preflight(_: &Paths, _: &std::path::Path) -> Result<(), String> {
    Err("prywatna namespace wymaga Linux".into())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn run<T: Serialize + DeserializeOwned>(
    _: &Paths,
    _: Entry<'_>,
    _: &[i32],
    _: impl FnOnce(&mut Worker<'_>) -> Result<T, String>,
    _: impl FnMut(&Anchor) -> Result<(), String>,
) -> Result<RunResult<T>, String> {
    Err("prywatna namespace wymaga Linux".into())
}
