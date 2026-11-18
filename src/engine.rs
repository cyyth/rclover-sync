//! Owns the rclone daemon and the sync policy: mounts, pinned folders and per-file states.

use crate::config::{self, Account, Config, Mode};
use crate::rclone::{self, Client, Rcd};
use gtk::glib;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub type Shared = Rc<RefCell<Engine>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileState {
    Cloud,
    Cached,
    Pinned,
    Syncing,
    Error,
}

impl FileState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloud => "cloud",
            Self::Cached => "cached",
            Self::Pinned => "pinned",
            Self::Syncing => "syncing",
            Self::Error => "error",
        }
    }
}

pub struct Engine {
    pub cfg: Config,
    pub client: Client,
    rcd: Option<Rcd>,
    cache_dir: PathBuf,
    /// Last `core/stats` reply.
    pub core: Value,
    /// Last `vfs/stats` reply per remote.
    pub vfs: HashMap<String, Value>,
    /// Last `core/transferred` list (completed transfers, oldest first).
    pub transferred: Vec<Value>,
    pub mount_errors: HashMap<String, String>,
    pub paused: bool,
    pub files_per_sec: u64,
    last_count: Option<u64>,
    syncing: HashSet<PathBuf>,
    errors: HashSet<PathBuf>,
    /// Accounts whose remount was deferred because uploads were pending.
    remount_later: HashSet<usize>,
    ticks: u64,
}

impl Engine {
    pub fn start() -> Result<Shared, String> {
        let cfg = config::load()?;
        let cache_dir = glib::user_cache_dir().join("nuvolo");
        let rcd = Rcd::spawn(cfg.rc_port, &cache_dir).map_err(|e| format!("could not start rclone: {e}"))?;
        let mut e = Engine {
            client: rcd.client.clone(),
            rcd: Some(rcd),
            cfg,
            cache_dir,
            core: Value::Null,
            vfs: HashMap::new(),
            transferred: Vec::new(),
            mount_errors: HashMap::new(),
            paused: false,
            files_per_sec: 0,
            last_count: None,
            syncing: HashSet::new(),
            errors: HashSet::new(),
            remount_later: HashSet::new(),
            ticks: 0,
        };
        e.apply_options();
        for i in 0..e.cfg.accounts.len() {
            e.mount(i);
        }
        e.refresh_pins();
        Ok(Rc::new(RefCell::new(e)))
    }

    /// Unmounts everything and stops rclone.
    pub fn shutdown(&mut self) {
        self.rcd.take();
    }

    pub fn save(&self) {
        if let Err(e) = config::save(&self.cfg) {
            eprintln!("nuvolo: could not save {}: {e}", config::path().display());
        }
    }

    pub fn apply_options(&self) {
        let opts = json!({ "main": { "Transfers": self.cfg.transfers, "Checkers": self.cfg.checkers } });
        if let Err(e) = self.client.call("options/set", opts) {
            eprintln!("nuvolo: options/set: {e}");
        }
    }

    pub fn mount(&mut self, i: usize) {
        let Some(a) = self.cfg.accounts.get(i) else { return };
        let res = rclone::mount(&self.client, &self.cfg, a);
        let remote = a.remote.clone();
        match res {
            Ok(()) => self.mount_errors.remove(&remote),
            Err(e) => self.mount_errors.insert(remote, e),
        };
    }

    pub fn pending_uploads(&self, remote: &str) -> u64 {
        self.vfs.get(remote).map_or(0, |v| {
            let d = &v["diskCache"];
            d["uploadsInProgress"].as_u64().unwrap_or(0) + d["uploadsQueued"].as_u64().unwrap_or(0)
        })
    }

    /// Remounts to apply filter or cache settings. While uploads are pending it waits and
    /// retries from `tick`, so queued writes are never cut off.
    pub fn remount(&mut self, i: usize) -> Result<(), String> {
        let a = self.cfg.accounts.get(i).ok_or("no such account")?;
        let pending = self.pending_uploads(&a.remote);
        if pending > 0 {
            self.remount_later.insert(i);
            return Err(format!("{pending} uploads pending; it will apply when they finish"));
        }
        self.remount_later.remove(&i);
        if self.client.call("mount/unmount", json!({ "mountPoint": a.mount })).is_err() {
            // Busy (e.g. open in Files): detach lazily; open handles keep the old view.
            let _ = std::process::Command::new("fusermount3").arg("-uz").arg(&a.mount).status();
        }
        self.mount(i);
        match self.mount_errors.get(&self.cfg.accounts[i].remote) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    pub fn add_account(&mut self, remote: &str) -> usize {
        self.cfg.accounts.push(Account::new(remote));
        self.save();
        let i = self.cfg.accounts.len() - 1;
        self.mount(i);
        i
    }

    /// Polls rclone. Returns the paths whose state changed since the last tick.
    pub fn tick(&mut self) -> Vec<PathBuf> {
        self.ticks += 1;
        if let Ok(v) = self.client.call("core/stats", json!({})) {
            let count = v["transfers"].as_u64().unwrap_or(0) + v["checks"].as_u64().unwrap_or(0);
            self.files_per_sec = self.last_count.map_or(0, |c| count.saturating_sub(c));
            self.last_count = Some(count);
            self.core = v;
        }
        for a in &self.cfg.accounts {
            if let Ok(v) = self.client.call("vfs/stats", json!({ "fs": a.fs() })) {
                self.vfs.insert(a.remote.clone(), v);
            }
        }
        if self.ticks % 5 == 1 {
            if let Ok(v) = self.client.call("core/transferred", json!({})) {
                self.transferred = v["transferred"].as_array().cloned().unwrap_or_default();
                let errors = self
                    .transferred
                    .iter()
                    .filter(|t| !t["error"].as_str().unwrap_or("").is_empty())
                    .filter_map(|t| self.transfer_path(t))
                    .collect();
                self.errors = errors;
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
        for i in self.remount_later.clone() {
            let _ = self.remount(i);
        }
        changed
    }

    /// Maps a `core/stats` transfer entry to an absolute path under its account's mount.
    fn transfer_path(&self, t: &Value) -> Option<PathBuf> {
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

    pub fn locate(&self, abs: &Path) -> Option<(usize, String)> {
        self.cfg
            .accounts
            .iter()
            .enumerate()
            .find_map(|(i, a)| abs.strip_prefix(&a.mount).ok().map(|r| (i, r.to_string_lossy().into_owned())))
    }

    pub fn state(&self, abs: &Path) -> Option<FileState> {
        let (i, rel) = self.locate(abs)?;
        let a = &self.cfg.accounts[i];
        Some(if self.syncing.iter().any(|p| p.starts_with(abs)) {
            FileState::Syncing
        } else if self.errors.contains(abs) {
            FileState::Error
        } else if a.mode_of(&rel) == Mode::Pinned {
            FileState::Pinned
        } else if self.cache_root(&a.remote).join(&rel).exists() {
            FileState::Cached
        } else {
            FileState::Cloud
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

    /// States of every entry in `dir`, keyed by absolute path.
    pub fn dir_states(&self, dir: &Path) -> HashMap<String, String> {
        if self.locate(dir).is_none() {
            return HashMap::new();
        }
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                self.state(&p).map(|s| (p.to_string_lossy().into_owned(), s.as_str().to_owned()))
            })
            .collect()
    }

    pub fn set_mode(&mut self, i: usize, rel: &str, mode: Mode) -> Result<(), String> {
        let rel = rel.trim_matches('/').to_owned();
        let a = self.cfg.accounts.get_mut(i).ok_or("add a cloud account first")?;
        let touches_filter = mode == Mode::Excluded || a.modes.get(&rel) == Some(&Mode::Excluded);
        if mode == Mode::OnDemand && rel.is_empty() {
            a.modes.remove("");
        } else {
            a.modes.insert(rel.clone(), mode);
        }
        let target = a.mount.join(&rel);
        self.save();
        if mode == Mode::Pinned {
            hydrate(target);
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

    /// Stops keeping `abs` on the device.
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
    }

    /// Re-lists the folder from the cloud now and, if pinned, re-reads it into the cache.
    pub fn sync_now(&self, abs: &Path) {
        let Some((i, rel)) = self.locate(abs) else { return };
        let a = &self.cfg.accounts[i];
        let dir = if abs.is_dir() {
            rel.clone()
        } else {
            Path::new(&rel).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
        };
        let (client, fs_, pinned, abs) = (self.client.clone(), a.fs(), a.mode_of(&rel) == Mode::Pinned, abs.to_path_buf());
        std::thread::spawn(move || {
            let mut params = json!({ "fs": fs_ });
            if !dir.is_empty() {
                params["dir"] = json!(dir);
            }
            if let Err(e) = client.call("vfs/refresh", params) {
                eprintln!("nuvolo: vfs/refresh: {e}");
            }
            if pinned {
                read_all(&abs);
            }
        });
    }

    /// Refreshes every pinned folder from the cloud and re-reads it, keeping it in the cache.
    // ponytail: "keep offline" relies on re-reading pinned files hourly so the LRU cache keeps
    // them; a cache smaller than the pinned set will still evict. Upgrade: per-file pin support in rclone.
    pub fn refresh_pins(&self) {
        let jobs: Vec<(String, String, PathBuf)> = self
            .cfg
            .accounts
            .iter()
            .filter(|a| a.background_sync)
            .flat_map(|a| {
                a.modes.iter().filter(|(_, m)| **m == Mode::Pinned).map(move |(p, _)| (a.fs(), p.clone(), a.mount.join(p)))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let client = self.client.clone();
        std::thread::spawn(move || {
            for (fs_, rel, abs) in jobs {
                let mut params = json!({ "fs": fs_, "recursive": true });
                if !rel.is_empty() {
                    params["dir"] = json!(rel);
                }
                let _ = client.call("vfs/refresh", params);
                read_all(&abs);
            }
        });
    }

    // ponytail: rclone has no global pause, so pausing throttles bandwidth to 1 byte/s.
    pub fn set_paused(&mut self, on: bool) -> Result<(), String> {
        self.client.call("core/bwlimit", json!({ "rate": if on { "1B" } else { "off" } }))?;
        self.paused = on;
        Ok(())
    }

    /// What a link lookup needs, so the network call can run off the main thread.
    pub fn link_job(&self, abs: &Path) -> Option<(Client, String, String)> {
        let (i, rel) = self.locate(abs)?;
        Some((self.client.clone(), self.cfg.accounts[i].remote.clone(), rel))
    }
}

/// Reads `path` through the mount in the background, which pulls it into the VFS cache.
pub fn hydrate(path: PathBuf) {
    std::thread::spawn(move || read_all(&path));
}

fn read_all(p: &Path) {
    let Ok(meta) = fs::symlink_metadata(p) else { return };
    if meta.is_dir() {
        for e in fs::read_dir(p).into_iter().flatten().flatten() {
            read_all(&e.path());
        }
    } else if meta.is_file() {
        if let Ok(mut f) = fs::File::open(p) {
            let _ = io::copy(&mut f, &mut io::sink());
        }
    }
}
