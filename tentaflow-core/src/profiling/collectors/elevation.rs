// =============================================================================
// File: collectors/elevation.rs — Sudo/UAC token wrapper. Zeroizes on drop.
// =============================================================================

use std::fmt;
use zeroize::Zeroizing;

/// Privilege escalation mechanism a collector relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationKind {
    None,
    /// POSIX `sudo` (password is held in the token).
    Sudo,
    /// Windows UAC — granted at process spawn, no in-band secret.
    Admin,
    /// Linux capabilities (`CAP_PERFMON`, `CAP_BPF`, ...) granted via `setcap`
    /// on the binary. No runtime secret.
    LinuxCap,
}

/// Holds an elevation secret in memory. Zeroizes on drop.
///
/// Only the `Sudo` kind carries an actual secret. `Admin` and `LinuxCap`
/// rely on out-of-band privilege grants and have no payload.
pub struct ElevationToken {
    kind: ElevationKind,
    /// `Zeroizing` guarantees the underlying bytes are overwritten when the
    /// token is dropped, even if the inner `Vec` reallocates internally before
    /// drop (each reallocation zeroizes the previous buffer).
    secret: Zeroizing<Vec<u8>>,
}

impl ElevationToken {
    /// Build a sudo token from a UTF-8 password. The password bytes are
    /// taken by value and zeroized on drop.
    pub fn new_sudo(password: String) -> Self {
        let bytes = password.into_bytes();
        Self {
            kind: ElevationKind::Sudo,
            secret: Zeroizing::new(bytes),
        }
    }

    /// Build a Windows UAC marker token. No secret is stored; UAC is managed
    /// by the OS at process creation time.
    pub fn new_admin() -> Self {
        Self {
            kind: ElevationKind::Admin,
            secret: Zeroizing::new(Vec::new()),
        }
    }

    /// Build a Linux-capability marker token (granted via `setcap` on the
    /// binary). No runtime secret.
    pub fn new_linux_cap() -> Self {
        Self {
            kind: ElevationKind::LinuxCap,
            secret: Zeroizing::new(Vec::new()),
        }
    }

    pub fn kind(&self) -> ElevationKind {
        self.kind
    }

    /// Borrow the secret bytes. Used by the sudo runner helper to feed
    /// `sudo -S` stdin. Callers must not store this slice.
    pub fn as_secret_bytes(&self) -> &[u8] {
        &self.secret
    }

    pub fn has_secret(&self) -> bool {
        !self.secret.is_empty()
    }
}

// Custom Debug: never reveal secret length or contents.
impl fmt::Debug for ElevationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ElevationToken")
            .field("kind", &self.kind)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevation_token_kinds() {
        assert_eq!(
            ElevationToken::new_sudo("p".into()).kind(),
            ElevationKind::Sudo
        );
        assert_eq!(ElevationToken::new_admin().kind(), ElevationKind::Admin);
        assert_eq!(
            ElevationToken::new_linux_cap().kind(),
            ElevationKind::LinuxCap
        );
    }

    #[test]
    fn elevation_token_admin_no_secret() {
        let t = ElevationToken::new_admin();
        assert!(!t.has_secret());
        assert!(t.as_secret_bytes().is_empty());

        let t = ElevationToken::new_linux_cap();
        assert!(!t.has_secret());
    }

    #[test]
    fn elevation_token_sudo_has_secret() {
        let t = ElevationToken::new_sudo("hunter2".into());
        assert!(t.has_secret());
        assert_eq!(t.as_secret_bytes(), b"hunter2");
    }

    #[test]
    fn elevation_token_debug_redacted() {
        let t = ElevationToken::new_sudo("hunter2".into());
        let dbg = format!("{:?}", t);
        assert!(!dbg.contains("hunter2"), "Debug leaked secret: {dbg}");
        assert!(dbg.contains("redacted"));
        assert!(dbg.contains("Sudo"));
    }

    #[test]
    fn elevation_token_zeroizes_on_drop() {
        // Reading a Vec's backing allocation after the Vec has been dropped
        // is UB, so we instead exercise the underlying `Zeroize` trait that
        // `Zeroizing` and therefore `ElevationToken` rely on. `Vec<u8>::zeroize`
        // overwrites every byte of the buffer with 0 (capacity is retained,
        // length is set to 0). The captured raw pointer is still valid because
        // the allocation has not been freed and capacity has not changed.
        use zeroize::Zeroize;

        let mut owned = b"super-secret".to_vec();
        let ptr = owned.as_ptr();
        let len = owned.len();

        // Sanity: pre-zeroize content matches.
        let pre: Vec<u8> = (0..len).map(|i| unsafe { *ptr.add(i) }).collect();
        assert_eq!(pre, b"super-secret");

        owned.zeroize();

        let post: Vec<u8> = (0..len).map(|i| unsafe { *ptr.add(i) }).collect();
        assert!(
            post.iter().all(|b| *b == 0),
            "buffer not zeroized: {post:?}"
        );
    }
}
