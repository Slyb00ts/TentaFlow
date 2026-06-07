// =============================================================================
// Plik: native.rs
// Opis: Wspólna logika odnajdywania katalogów native-libs dla wrapperów.
// Przykład: let layout = NativeLayout::discover()?;
// =============================================================================

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativePlatform {
    LinuxX86_64,
    LinuxAarch64,
    MacosX86_64,
    MacosArm64,
    WindowsX86_64,
}

impl NativePlatform {
    pub fn detect() -> Result<Self, NativeError> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => Ok(Self::LinuxX86_64),
            ("linux", "aarch64") => Ok(Self::LinuxAarch64),
            ("macos", "x86_64") => Ok(Self::MacosX86_64),
            ("macos", "aarch64") => Ok(Self::MacosArm64),
            ("windows", "x86_64") => Ok(Self::WindowsX86_64),
            (os, arch) => Err(NativeError::UnsupportedPlatform {
                os: os.to_string(),
                arch: arch.to_string(),
            }),
        }
    }

    pub fn as_dir_name(&self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "linux-x86_64",
            Self::LinuxAarch64 => "linux-aarch64",
            Self::MacosX86_64 => "macos-x86_64",
            Self::MacosArm64 => "macos-arm64",
            Self::WindowsX86_64 => "windows-x86_64",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NativeLayout {
    root: PathBuf,
    platform: NativePlatform,
}

impl NativeLayout {
    pub fn discover() -> Result<Self, NativeError> {
        let platform = NativePlatform::detect()?;
        let root = if let Ok(value) = std::env::var("TENTAFLOW_NATIVE_LIBS_DIR") {
            PathBuf::from(value)
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../native-libs")
        };
        let layout = Self { root, platform };
        layout.require_platform_dir()?;
        Ok(layout)
    }

    pub fn with_root(root: impl Into<PathBuf>, platform: NativePlatform) -> Self {
        Self {
            root: root.into(),
            platform,
        }
    }

    pub fn platform(&self) -> &NativePlatform {
        &self.platform
    }

    pub fn platform_dir(&self) -> PathBuf {
        self.root.join(self.platform.as_dir_name())
    }

    pub fn include_dir(&self) -> PathBuf {
        self.platform_dir().join("include")
    }

    pub fn static_dir(&self) -> PathBuf {
        self.platform_dir().join("lib-static")
    }

    pub fn dynamic_dir(&self) -> PathBuf {
        self.platform_dir().join("lib-dynamic")
    }

    pub fn require_file(&self, path: impl AsRef<Path>) -> Result<PathBuf, NativeError> {
        let path = path.as_ref().to_path_buf();
        if path.is_file() {
            Ok(path)
        } else {
            Err(NativeError::MissingFile(path))
        }
    }

    fn require_platform_dir(&self) -> Result<(), NativeError> {
        let dir = self.platform_dir();
        if dir.is_dir() {
            Ok(())
        } else {
            Err(NativeError::MissingPlatformDir(dir))
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    #[error("nieobsługiwana platforma native-libs: {os}-{arch}")]
    UnsupportedPlatform { os: String, arch: String },
    #[error("brak katalogu native-libs dla platformy: {0}")]
    MissingPlatformDir(PathBuf),
    #[error("brak wymaganego pliku native-libs: {0}")]
    MissingFile(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::{NativeLayout, NativePlatform};

    #[test]
    fn platform_dir_uses_expected_name() {
        let layout = NativeLayout::with_root("/repo/native-libs", NativePlatform::LinuxX86_64);

        assert_eq!(
            layout.platform_dir().to_string_lossy(),
            "/repo/native-libs/linux-x86_64"
        );
    }
}
