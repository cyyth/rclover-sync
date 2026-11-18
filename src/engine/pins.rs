//! Per-folder modes: On-Demand, Always Offline (pinned) and Excluded, and keeping pinned
//! files warm in rclone's VFS cache so the live mount can open them without the network.
//! The offline copies that work without Nuvolo are in [`super::offline`].

use super::Engine;
use crate::config::Mode;
use serde_json::json;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
        let target = a.mount.join(&rel);
        self.save();
        if mode == Mode::Pinned {
            hydrate(target, self.cfg.transfers);
        }
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

    /// Re-lists the folder from the cloud now and, if pinned, re-reads it into the cache and
    /// syncs its offline copy.
    pub fn sync_now(&mut self, abs: &Path) {
        let Some((i, rel)) = self.locate(abs) else { return };
        let a = &self.cfg.accounts[i];
        let pinned = a.mode_of(&rel) == Mode::Pinned;
        let (client, fs_, abs, workers) = (self.client.clone(), a.fs(), abs.to_path_buf(), self.cfg.transfers);
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
            if pinned {
                read_all(&abs, workers);
            }
        });
        if pinned {
            self.sync_offline(i);
        }
    }

    /// Refreshes every pinned folder from the cloud and re-reads it, keeping it in the cache.
    // ponytail: keeping pinned files in the VFS cache relies on re-reading them hourly so the
    // LRU cache keeps them; a cache smaller than the pinned set will still evict (the offline
    // copy is unaffected). Upgrade: per-file pin support in rclone.
    pub fn refresh_pins(&self) {
        let jobs: Vec<(String, String, PathBuf)> = self
            .cfg
            .accounts
            .iter()
            .filter(|a| a.background_sync)
            .flat_map(|a| a.modes.iter().filter(|(_, m)| **m == Mode::Pinned).map(move |(p, _)| (a.fs(), p.clone(), a.mount.join(p))))
            .collect();
        if jobs.is_empty() {
            return;
        }
        let (client, workers) = (self.client.clone(), self.cfg.transfers);
        std::thread::spawn(move || {
            for (fs_, rel, abs) in jobs {
                let mut params = json!({ "fs": fs_, "recursive": "true" });
                if !rel.is_empty() {
                    params["dir"] = json!(rel);
                }
                let _ = client.call("vfs/refresh", params);
                read_all(&abs, workers);
            }
        });
    }
}

/// Reads `path` through the mount in the background, which pulls it into the VFS cache.
pub fn hydrate(path: PathBuf, workers: u32) {
    std::thread::spawn(move || read_all(&path, workers));
}

/// Reads every file under `root` with `workers` parallel readers, so pinning a folder
/// downloads as many files at once as rclone is allowed to transfer.
fn read_all(root: &Path, workers: u32) {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let Ok(meta) = fs::symlink_metadata(&d) else { continue };
        if meta.is_file() {
            files.push(d);
        } else if meta.is_dir() {
            dirs.extend(fs::read_dir(&d).into_iter().flatten().flatten().map(|e| e.path()));
        }
    }
    let queue = std::sync::Mutex::new(files);
    std::thread::scope(|s| {
        for _ in 0..workers.max(1) {
            s.spawn(|| loop {
                let Some(f) = queue.lock().unwrap_or_else(|e| e.into_inner()).pop() else { break };
                if let Ok(mut file) = fs::File::open(&f) {
                    let _ = io::copy(&mut file, &mut io::sink());
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_all_reads_every_file_in_parallel() {
        let dir = std::env::temp_dir().join(format!("nuvolo-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("a/b")).unwrap();
        for f in ["x", "a/y", "a/b/z"] {
            fs::write(dir.join(f), b"data").unwrap();
        }
        read_all(&dir, 3); // must terminate and not panic
        fs::remove_dir_all(dir).unwrap();
    }
}
