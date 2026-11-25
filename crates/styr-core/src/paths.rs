//! Where the daemon keeps its socket and database.
//!
//! `STYR_HOME` overrides the data dir and `STYR_SOCKET` the socket path (tests,
//! multiple instances; macOS caps socket paths at 103 bytes). Otherwise:
//! - macOS: `~/Library/Application Support/Styrman/`
//! - Linux: `$XDG_DATA_HOME/styrman` (or `~/.local/share/styrman`) for data, and
//!   `$XDG_RUNTIME_DIR/styrman` for the socket when that is set.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    pub db: PathBuf,
}

impl Paths {
    pub fn in_dir(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        Self {
            socket: dir.join("styrd.sock"),
            db: dir.join("styrd.sqlite"),
            data_dir: dir,
        }
    }

    /// Default locations, then `STYR_HOME` (data dir) and `STYR_SOCKET` (socket path) overrides.
    pub fn resolve() -> Self {
        let mut p = Self::resolve_dirs();
        if let Some(sock) = std::env::var_os("STYR_SOCKET").filter(|v| !v.is_empty()) {
            p.socket = PathBuf::from(sock);
        }
        p
    }

    fn resolve_dirs() -> Self {
        if let Some(home) = std::env::var_os("STYR_HOME").filter(|v| !v.is_empty()) {
            return Self::in_dir(home);
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        if cfg!(target_os = "macos") {
            return Self::in_dir(home.join("Library/Application Support/Styrman"));
        }
        let data = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("styrman");
        let mut p = Self::in_dir(&data);
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            p.socket = PathBuf::from(rt).join("styrman/styrd.sock");
        }
        p
    }
}
