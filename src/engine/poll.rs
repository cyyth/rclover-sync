//! The once-a-second refresh. [`Engine::poll_job`] hands out the slow part (rclone calls and
//! mount checks) to run on a worker thread; [`Engine::apply`] takes its [`Poll`] back on the
//! main thread and does everything else: statistics, finished background work, healing,
//! periodic syncs. A busy rclone or a slow mount therefore never freezes the window.

use super::{mounts::MountState, Engine};
use crate::config::Account;
use crate::rclone::{self, Client};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

/// What one poll of rclone found.
pub struct Poll {
    core: Option<Value>,
    vfs: Vec<(String, Value)>,
    transferred: Option<Vec<Value>>,
    /// Accounts whose mount is missing or dead, when this poll checked.
    unhealthy: Option<HashSet<usize>>,
}

/// Uploads waiting or running in a `vfs/stats` reply.
pub fn uploads_in(v: &Value) -> u64 {
    let d = &v["diskCache"];
    d["uploadsInProgress"].as_u64().unwrap_or(0) + d["uploadsQueued"].as_u64().unwrap_or(0)
}

impl Engine {
    /// Starts a tick: returns the slow part to run off the main thread.
    pub fn poll_job(&mut self) -> impl FnOnce() -> Poll + Send + 'static {
        self.ticks += 1;
        self.ensure_rclone();
        let client: Client = self.client.clone();
        // (remote, fs, mount point, mounted as far as Nuvolo knows)
        let accounts: Vec<(String, String, PathBuf, bool)> = self
            .cfg
            .accounts
            .iter()
            .enumerate()
            .map(|(i, a)| (a.remote.clone(), a.fs(), a.mount.clone(), self.mount_state(i) == MountState::Mounted))
            .collect();
        let (want_transferred, check_mounts) = (self.ticks % 5 == 1, self.ticks % 5 == 0);
        move || {
            let vfs = accounts
                .iter()
                .filter(|a| a.3)
                .filter_map(|(remote, fs_, _, _)| client.call("vfs/stats", json!({ "fs": fs_ })).ok().map(|v| (remote.clone(), v)))
                .collect();
            let unhealthy = check_mounts.then(|| {
                let mounted = rclone::mounted_paths();
                // Listed is not enough: a dead mount stays listed but every access fails.
                (0..accounts.len()).filter(|&i| !mounted.contains(&accounts[i].2) || rclone::is_dead_mount(&accounts[i].2)).collect()
            });
            Poll {
                core: client.call("core/stats", json!({})).ok(),
                vfs,
                transferred: want_transferred
                    .then(|| client.call("core/transferred", json!({})).ok())
                    .flatten()
                    .map(|v| v["transferred"].as_array().cloned().unwrap_or_default()),
                unhealthy,
            }
        }
    }

    /// Finishes a tick with what [`Engine::poll_job`] found. Returns the paths whose state changed.
    pub fn apply(&mut self, p: Poll) -> Vec<PathBuf> {
        self.drain_done();
        if let Some(v) = p.core {
            let count = v["transfers"].as_u64().unwrap_or(0) + v["checks"].as_u64().unwrap_or(0);
            self.files_per_sec = self.last_count.map_or(0, |c| count.saturating_sub(c));
            self.last_count = Some(count);
            let now = std::time::Instant::now();
            let bytes = v["bytes"].as_f64().unwrap_or(0.0);
            self.speed = match self.last_bytes {
                Some((b, t)) if !self.paused => ((bytes - b) / now.duration_since(t).as_secs_f64().max(0.001)).max(0.0),
                _ => 0.0,
            };
            self.last_bytes = Some((bytes, now));
            self.core = v;
        }
        self.vfs.extend(p.vfs);
        if let Some(list) = p.transferred {
            if list != self.transferred {
                self.transferred = list;
                self.transferred_gen += 1;
                self.errors = self
                    .transferred
                    .iter()
                    .filter(|t| !t["error"].as_str().unwrap_or("").is_empty())
                    .filter_map(|t| self.transfer_path(t))
                    .collect();
            }
        }
        let now: HashSet<PathBuf> = self.core["transferring"]
            .as_array()
            .map(|l| l.iter().filter_map(|t| self.transfer_path(t)).collect())
            .unwrap_or_default();
        let changed = now.symmetric_difference(&self.syncing).cloned().collect();
        self.syncing = now;
        if self.ticks % 3600 == 0 {
            self.refresh_pins();
        }
        if self.ticks % 600 == 0 {
            for i in 0..self.cfg.accounts.len() {
                self.sync_offline(i);
            }
        }
        if let Some(unhealthy) = p.unhealthy {
            for i in self.remount_later.clone() {
                let _ = self.remount(i);
            }
            self.heal_mounts(&unhealthy);
        }
        changed
    }

    /// Maps a `core/stats` transfer entry to an absolute path under its account's mount.
    pub(super) fn transfer_path(&self, t: &Value) -> Option<PathBuf> {
        let name = t["name"].as_str()?;
        let on = |a: &&Account| {
            [&t["srcFs"], &t["dstFs"]]
                .iter()
                .any(|f| f.as_str().is_some_and(|s| s == a.fs() || s.trim_end_matches(':') == a.remote))
        };
        let a = self.cfg.accounts.iter().find(on).or(match self.cfg.accounts.as_slice() {
            [only] => Some(only),
            _ => None,
        })?;
        Some(a.mount.join(name))
    }
}
