//! The adaptive refresh. [`Engine::poll_job`] hands out the slow part (rclone calls and
//! mount checks) to run on a worker thread; [`Engine::apply`] takes its [`Poll`] back on the
//! main thread and does everything else: statistics, finished background work, watchdog,
//! healing, periodic syncs. A busy rclone or a slow mount therefore never freezes the window.

use super::{mounts::MountState, Engine, WATCHDOG_FAILURES};
use crate::config::Account;
use crate::rclone::{self, MountHealth};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// What one poll of rclone found.
pub struct Poll {
    /// True if this poll actually queried rclone (for the watchdog).
    checked_rclone: bool,
    core: Option<Value>,
    vfs: Vec<(String, Value)>,
    transferred: Option<Vec<Value>>,
    /// Mount health per account that is missing, dead, or unhealthy, when this poll checked.
    unhealthy: Option<HashMap<usize, MountHealth>>,
}

/// Uploads waiting or running in a `vfs/stats` reply.
pub fn uploads_in(v: &Value) -> u64 {
    let d = &v["diskCache"];
    d["uploadsInProgress"].as_u64().unwrap_or(0) + d["uploadsQueued"].as_u64().unwrap_or(0)
}

impl Engine {
    /// Starts a tick: returns the slow part to run off the main thread.
    /// `want_stats` controls whether full `core/stats` + `vfs/stats` are fetched (adaptive rate);
    /// health checks and watchdog still run every 5 s regardless.
    pub fn poll_job(&mut self, want_stats: bool) -> impl FnOnce() -> Poll + Send + 'static {
        self.ticks += 1;
        self.drain_done();
        self.ensure_rclone();
        let client = self.client.clone();
        let accounts: Vec<(String, String, PathBuf, bool)> = self
            .cfg
            .accounts
            .iter()
            .enumerate()
            .map(|(i, a)| (a.remote.clone(), a.fs(), a.mount.clone(), self.mount_state(i) == MountState::Mounted))
            .collect();
        let check_mounts = self.ticks % 5 == 0;
        let want_transferred = want_stats && self.ticks % 5 == 1;
        let need_core = want_stats || check_mounts;
        move || {
            let Some(client) = client else {
                return Poll {
                    checked_rclone: false,
                    core: None,
                    vfs: Vec::new(),
                    transferred: None,
                    unhealthy: None,
                };
            };
            let core = if need_core {
                client.call_local("core/stats", json!({})).ok()
            } else {
                None
            };
            let vfs = if want_stats {
                accounts
                    .iter()
                    .filter(|a| a.3)
                    .filter_map(|(remote, fs_, _, _)| client.call_local("vfs/stats", json!({ "fs": fs_ })).ok().map(|v| (remote.clone(), v)))
                    .collect()
            } else {
                Vec::new()
            };
            let unhealthy = check_mounts.then(|| {
                let mounted = rclone::mounted_paths();
                let mut bad = HashMap::new();
                for (i, (_, _, mount, _)) in accounts.iter().enumerate() {
                    if !rclone::is_mounted_path(&mounted, mount) {
                        bad.insert(i, MountHealth::Dead);
                    } else {
                        let health = rclone::probe_mount(&rclone::canonical_mount_path(mount), Duration::from_secs(5));
                        if health != MountHealth::Healthy {
                            bad.insert(i, health);
                        }
                    }
                }
                bad
            });
            Poll {
                checked_rclone: need_core,
                core,
                vfs,
                transferred: want_transferred
                    .then(|| client.call_local("core/transferred", json!({})).ok())
                    .flatten()
                    .map(|v| v["transferred"].as_array().cloned().unwrap_or_default()),
                unhealthy,
            }
        }
    }

    /// Finishes a tick with what [`Engine::poll_job`] found. Returns the paths whose state changed.
    pub fn apply(&mut self, p: Poll) -> Vec<PathBuf> {
        self.drain_done();
        if p.checked_rclone && !self.rclone_starting && self.rcd.is_some() {
            if p.core.is_some() {
                self.poll_failures = 0;
            } else {
                self.poll_failures += 1;
                if self.poll_failures >= WATCHDOG_FAILURES {
                    eprintln!("rclover: rclone unresponsive after {} polls, restarting", self.poll_failures);
                    self.spawn_rclone();
                }
            }
        }
        if let Some(v) = p.core {
            let now = Instant::now();
            let count = v["transfers"].as_u64().unwrap_or(0) + v["checks"].as_u64().unwrap_or(0);
            self.files_per_sec = match self.last_count {
                Some((c, t)) => {
                    let dt = now.duration_since(t).as_secs_f64().max(0.5);
                    ((count.saturating_sub(c)) as f64 / dt).round() as u64
                }
                None => 0,
            };
            self.last_count = Some((count, now));
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
        let now: std::collections::HashSet<PathBuf> = self.core["transferring"]
            .as_array()
            .map(|l| l.iter().filter_map(|t| self.transfer_path(t)).collect())
            .unwrap_or_default();
        let changed = now.symmetric_difference(&self.syncing).cloned().collect();
        self.syncing = now;
        // Stagger periodic offline syncs across accounts (30 s apart) instead of launching all at once.
        let due: Vec<usize> = (0..self.cfg.accounts.len())
            .filter(|&i| self.cfg.accounts[i].background_sync && (self.ticks + i as u64 * 30) % 600 == 0)
            .collect();
        for i in due {
            self.sync_offline(i);
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
