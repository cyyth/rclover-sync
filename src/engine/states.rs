//! The per-file state shown as emblems in GNOME Files (see docs/dbus.rst).

use super::Engine;
use crate::config::Mode;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileState {
    Cloud,
    Cached,
    /// A folder with some pinned or cached content that is not pinned itself.
    Partial,
    Pinned,
    Syncing,
    Error,
}

impl FileState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloud => "cloud",
            Self::Cached => "cached",
            Self::Partial => "partial",
            Self::Pinned => "pinned",
            Self::Syncing => "syncing",
            Self::Error => "error",
        }
    }
}

impl Engine {
    /// The state of `abs`. `is_dir` comes from the caller, which has looked at the file off the
    /// main thread: a stat through the mount can take as long as a network round trip.
    pub fn state(&self, abs: &Path, is_dir: bool) -> Option<FileState> {
        let (i, rel) = self.locate(abs)?;
        let a = &self.cfg.accounts[i];
        Some(if self.syncing.iter().any(|p| p.starts_with(abs)) {
            FileState::Syncing
        } else if self.errors.contains(abs) {
            FileState::Error
        } else if a.mode_of(&rel) == Mode::Pinned {
            FileState::Pinned
        } else {
            let cached = self.cache_root(&a.remote).join(&rel);
            match (is_dir, cached.exists()) {
                (true, has_cache) if has_cache || a.has_pinned_below(&rel) => FileState::Partial,
                (false, true) => FileState::Cached,
                _ => FileState::Cloud,
            }
        })
    }

    /// Where rclone keeps cached data for a remote. rclone names this folder after the
    /// underlying backend (an alias or crypt remote differs from its name), so ask it.
    fn cache_root(&self, remote: &str) -> PathBuf {
        self.vfs
            .get(remote)
            .and_then(|v| v["diskCache"]["path"].as_str())
            .map_or_else(|| self.cache_dir.join("vfs").join(remote), PathBuf::from)
    }
}
