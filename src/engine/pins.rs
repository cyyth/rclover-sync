//! Per-folder modes: On-Demand, Always Offline (pinned) and Excluded.
//!
//! Pinned content lives as offline copies ([`super::offline`]); it is not loaded into rclone's
//! VFS cache, which is only the space for files opened on demand.

use super::Engine;
use crate::config::Mode;
use serde_json::json;
use std::path::Path;

impl Engine {
    pub fn set_mode(&mut self, i: usize, rel: &str, mode: Mode) -> Result<(), String> {
        let rel = rel.trim_matches('/').to_owned();
        let a = self.cfg.accounts.get_mut(i).ok_or("add a cloud account first")?;
        let touches_filter = mode == Mode::Excluded || a.modes.get(&rel) == Some(&Mode::Excluded);
        let was_pinned = a.mode_of(&rel) == Mode::Pinned;
        if mode == Mode::OnDemand && rel.is_empty() {
            a.modes.remove("");
        } else {
            a.modes.insert(rel.clone(), mode);
        }
        self.save();
        if mode == Mode::Pinned || was_pinned {
            self.sync_offline(i); // makes or removes the offline copy
        }
        if touches_filter {
            self.remount(i)?;
        }
        Ok(())
    }

    pub fn pin(&mut self, abs: &Path) {
        if let Some((i, rel)) = self.locate(abs) {
            if let Err(e) = self.set_mode(i, &rel, Mode::Pinned) {
                eprintln!("nuvolo: pin {}: {e}", abs.display());
            }
        }
    }

    /// Stops keeping `abs` on the device. Its offline copy is removed by the next offline sync
    /// (after anything changed in it has been moved into the drive).
    // ponytail: rclone has no rc call to evict a single file from the VFS cache, so this only
    // unpins and leaves eviction to the LRU cleaner (cache max age / size). Upgrade path: a
    // `vfs/evict` rc call upstream, then call it here.
    pub fn free(&mut self, abs: &Path) {
        let Some((i, rel)) = self.locate(abs) else { return };
        let a = &mut self.cfg.accounts[i];
        if a.modes.get(&rel) == Some(&Mode::Pinned) {
            a.modes.remove(&rel);
        }
        if a.mode_of(&rel) == Mode::Pinned {
            a.modes.insert(rel, Mode::OnDemand);
        }
        self.save();
        self.sync_offline(i);
    }

    /// Re-lists the folder from the cloud now and, if pinned, syncs its offline copy.
    pub fn sync_now(&mut self, abs: &Path) {
        let Some((i, rel)) = self.locate(abs) else { return };
        let a = &self.cfg.accounts[i];
        let pinned = a.mode_of(&rel) == Mode::Pinned;
        let (client, fs_, abs) = (self.client.clone(), a.fs(), abs.to_path_buf());
        std::thread::spawn(move || {
            let dir = if abs.is_dir() {
                rel.clone()
            } else {
                Path::new(&rel).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
            };
            let mut params = json!({ "fs": fs_ });
            if !dir.is_empty() {
                params["dir"] = json!(dir);
            }
            if let Err(e) = client.call("vfs/refresh", params) {
                eprintln!("nuvolo: vfs/refresh: {e}");
            }
        });
        if pinned {
            self.sync_offline(i);
        }
    }
}
