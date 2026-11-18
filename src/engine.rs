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
    /// Bytes per second over the last tick (rclone's own speed is a session average).
    pub speed: f64,
    last_count: Option<u64>,
    last_bytes: Option<(f64, std::time::Instant)>,
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
            speed: 0.0,
            last_count: None,
            last_bytes: None,
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

    /// Remounts to apply filter or cache settings. While uploads are pending or the mount is
    /// busy (open in Files), it retries from `tick` instead.
    ///
    /// Never detach a busy mount lazily: when the last user of the old mount lets go, rclone's
    /// cleanup unmounts the mount point, which by then is the new mount (issue #1).
    pub fn remount(&mut self, i: usize) -> Result<(), String> {
        let a = self.cfg.accounts.get(i).ok_or("no such account")?;
        let pending = self.pending_uploads(&a.remote);
        if pending > 0 {
            self.remount_later.insert(i);
            return Err(format!("{pending} uploads pending; it will apply when they finish"));
        }
        if self.client.call("mount/unmount", json!({ "mountPoint": a.mount })).is_err() {
            self.remount_later.insert(i);
            return Err("the drive is in use; it will apply once no window has it open".into());
        }
        self.remount_later.remove(&i);
        self.mount(i);
        match self.mount_errors.get(&self.cfg.accounts[i].remote) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    /// Remounts any account whose mount has disappeared (crash of a FUSE helper, a manual
    /// unmount, a suspend glitch), so ~/Cloud never silently goes empty.
    fn heal_mounts(&mut self) {
        let mounted = mounted_paths();
        for i in 0..self.cfg.accounts.len() {
            let a = &self.cfg.accounts[i];
            if self.remount_later.contains(&i) || mounted.contains(&a.mount) {
                continue;
            }
            // A failing mount is retried every 30 s, a lost one right away.
            if self.mount_errors.contains_key(&a.remote) && self.ticks % 30 != 0 {
                continue;
            }
            eprintln!("nuvolo: {} is not mounted, mounting again", a.mount.display());
            let _ = self.client.call("mount/unmount", json!({ "mountPoint": a.mount }));
            self.mount(i);
        }
    }

    pub fn is_mounted(&self, i: usize) -> bool {
        self.cfg.accounts.get(i).is_some_and(|a| mounted_paths().contains(&a.mount))
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
            let now = std::time::Instant::now();
            let bytes = v["bytes"].as_f64().unwrap_or(0.0);
            self.speed = match self.last_bytes {
                Some((b, t)) if !self.paused => ((bytes - b) / now.duration_since(t).as_secs_f64().max(0.001)).max(0.0),
                _ => 0.0,
            };
            self.last_bytes = Some((bytes, now));
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
        if self.ticks % 5 == 0 {
            for i in self.remount_later.clone() {
                let _ = self.remount(i);
            }
            self.heal_mounts();
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
        } else {
            let cached = self.cache_root(&a.remote).join(&rel);
            match (abs.is_dir(), cached.exists()) {
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
            hydrate(target, self.cfg.transfers);
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
        let (client, fs_, pinned, abs, workers) = (self.client.clone(), a.fs(), a.mode_of(&rel) == Mode::Pinned, abs.to_path_buf(), self.cfg.transfers);
        std::thread::spawn(move || {
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
        let (client, workers) = (self.client.clone(), self.cfg.transfers);
        std::thread::spawn(move || {
            for (fs_, rel, abs) in jobs {
                let mut params = json!({ "fs": fs_, "recursive": true });
                if !rel.is_empty() {
                    params["dir"] = json!(rel);
                }
                let _ = client.call("vfs/refresh", params);
                read_all(&abs, workers);
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

/// Mount points currently listed in /proc/self/mounts.
fn mounted_paths() -> HashSet<PathBuf> {
    parse_mounts(&fs::read_to_string("/proc/self/mounts").unwrap_or_default())
}

fn parse_mounts(text: &str) -> HashSet<PathBuf> {
    text.lines()
        .filter_map(|l| l.split(' ').nth(1))
        // The kernel escapes spaces, tabs, newlines and backslashes as octal.
        .map(|p| PathBuf::from(p.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_escaped_mount_points() {
        let m = parse_mounts("gdrive: /home/me/Cloud/My\\040Drive fuse.rclone rw 0 0\nproc /proc proc rw 0 0\n");
        assert!(m.contains(Path::new("/home/me/Cloud/My Drive")));
        assert!(m.contains(Path::new("/proc")));
    }

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
