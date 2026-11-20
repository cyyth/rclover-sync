//! The engine: owns the rclone daemon and Nuvolo's sync policy.
//!
//! It lives on the main thread ([`Shared`]) and never waits on the network there: slow work
//! (mounting, two-way sync, polling rclone) runs on threads, and the results come back as
//! [`Done`] messages or a [`Poll`], applied once a second by [`Engine::apply`].
//!
//! - [`poll`]: the once-a-second refresh of rclone's statistics.
//! - [`mounts`]: mounting, remounting and healing mounts; retrying when the network returns.
//! - [`offline`]: offline copies of pinned folders, kept under the mount point.
//! - [`pins`]: per-folder modes (on-demand, offline, excluded) and cache warming.
//! - [`states`]: the per-file state shown as emblems in GNOME Files.

mod mounts;
mod offline;
mod pins;
mod poll;
mod states;

pub use mounts::{MountState, UnmountKind};
pub use offline::OfflineProblem;
pub use poll::uploads_in;

use crate::config::{self, Account, Config};
use crate::rclone::{self, Client, Rcd, RcloneCaps};
use gtk::glib;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

pub type Shared = Rc<RefCell<Engine>>;

/// Consecutive rclone poll failures/timeouts before the watchdog restarts `rclone rcd`.
const WATCHDOG_FAILURES: u32 = 4;

/// A result from background work, sent back to the main thread.
/// Every variant carries the `rclone_gen` it was started under so stale results from a
/// previous rclone instance are ignored.
pub enum Done {
    /// Starting or restarting `rclone rcd` finished.
    RcloneReady(u64, Result<Rcd, String>),
    /// Mounting account `i` finished, optionally carrying the newly held offline-dir handle.
    Mounted(u64, usize, Result<(), String>, Option<File>),
    /// Unmounting account `i` finished.
    Unmounted(u64, usize, Result<(), String>, UnmountKind),
    /// Syncing the offline copies of account `i` finished; with their size on disk.
    Synced(u64, usize, Result<(), OfflineProblem>, u64),
    /// `vfs/refresh` for `sync_now` finished; pinned folders can now run bisync.
    Refreshed(u64, usize),
    /// `core/bwlimit` pause/resume finished.
    Paused(u64, bool, Result<(), String>),
}

/// A home-folder move that is running now (see `homedirs::runner`).
pub struct Progress {
    pub op: crate::homedirs::Op,
    /// Copying is done and the files are waiting to upload; `count` is then the uploads left.
    pub uploading: bool,
    /// Files copied so far.
    pub count: Arc<AtomicUsize>,
}

pub struct Engine {
    pub cfg: Config,
    pub client: Option<Client>,
    rcd: Option<Rcd>,
    caps: RcloneCaps,
    rclone_gen: u64,
    pub rclone_starting: bool,
    pub rclone_error: Option<String>,
    poll_failures: u32,
    cache_dir: PathBuf,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    stopped: bool,
    ticks: u64,

    // Statistics, from `poll`.
    /// Last `core/stats` reply.
    pub core: Value,
    /// Last `vfs/stats` reply per remote.
    pub vfs: HashMap<String, Value>,
    /// Last `core/transferred` list (completed transfers, oldest first).
    pub transferred: Vec<Value>,
    /// Bumped whenever `transferred` changes, so the window redraws lists only then.
    pub transferred_gen: u64,
    pub files_per_sec: u64,
    /// Bytes per second over the last tick (rclone's own speed is a session average).
    pub speed: f64,
    last_count: Option<(u64, Instant)>,
    last_bytes: Option<(f64, Instant)>,
    syncing: HashSet<PathBuf>,
    errors: HashSet<PathBuf>,
    pub paused: bool,

    // Mounts, from `mounts`.
    /// Why an account is not mounted, by remote.
    mount_errors: HashMap<String, String>,
    /// Accounts being mounted now.
    mounting: HashSet<usize>,
    /// Accounts being unmounted now.
    unmounting: HashSet<usize>,
    /// Accounts whose remount was deferred because uploads were pending.
    remount_later: HashSet<usize>,
    /// Consecutive EIO / timeout mount-health strikes per account.
    unhealthy_strikes: HashMap<usize, u32>,

    // Offline copies, from `offline`.
    /// An open handle on each account's mount folder, taken before mounting.
    offline_dirs: HashMap<usize, File>,
    /// Accounts whose offline copies are syncing now.
    offline_running: HashSet<usize>,
    /// Why the last sync of an account's offline copies failed, by remote.
    offline_errors: HashMap<String, OfflineProblem>,
    /// Size of each account's offline copies after the last sync, by remote.
    offline_bytes: HashMap<String, u64>,

    /// Home-folder moves running now, by home folder.
    pub jobs: HashMap<PathBuf, Progress>,
}

impl Engine {
    /// Loads configuration and starts `rclone rcd` in the background. Returns at once so the
    /// window opens immediately without waiting up to 10 s for rclone.
    pub fn start() -> Result<Shared, String> {
        let cfg = config::load()?;
        let cache_dir = glib::user_cache_dir().join("nuvolo");
        let (done_tx, done_rx) = channel();
        let mut e = Engine {
            client: None,
            rcd: None,
            caps: RcloneCaps::default(),
            rclone_gen: 0,
            rclone_starting: false,
            rclone_error: None,
            poll_failures: 0,
            cfg,
            cache_dir,
            done_tx,
            done_rx,
            stopped: false,
            ticks: 0,
            core: Value::Null,
            vfs: HashMap::new(),
            transferred: Vec::new(),
            transferred_gen: 0,
            files_per_sec: 0,
            speed: 0.0,
            last_count: None,
            last_bytes: None,
            syncing: HashSet::new(),
            errors: HashSet::new(),
            paused: false,
            mount_errors: HashMap::new(),
            mounting: HashSet::new(),
            unmounting: HashSet::new(),
            remount_later: HashSet::new(),
            unhealthy_strikes: HashMap::new(),
            offline_dirs: HashMap::new(),
            offline_running: HashSet::new(),
            offline_errors: HashMap::new(),
            offline_bytes: HashMap::new(),
            jobs: HashMap::new(),
        };
        e.spawn_rclone();
        Ok(Rc::new(RefCell::new(e)))
    }

    /// Starts or restarts `rclone rcd` on a worker thread, reporting back via [`Done::RcloneReady`].
    pub(super) fn spawn_rclone(&mut self) {
        if self.stopped || self.rclone_starting {
            return;
        }
        self.rclone_starting = true;
        self.rclone_gen += 1;
        let gen = self.rclone_gen;
        let old = self.rcd.take();
        self.client = None;
        self.paused = false;
        self.mounting.clear();
        self.unmounting.clear();
        self.offline_running.clear();
        self.poll_failures = 0;
        let port = self.cfg.rc_port;
        let cache_dir = self.cache_dir.clone();
        let tx = self.done_tx.clone();
        std::thread::spawn(move || {
            // Drop any previous Rcd off the GTK thread (Drop waits for unmountall + exit).
            drop(old);
            let res = Rcd::spawn(port, &cache_dir).map_err(|e| format!("could not start rclone: {e}"));
            let _ = tx.send(Done::RcloneReady(gen, res));
        });
    }

    /// Retries starting rclone immediately (for the Retry button when startup failed).
    pub fn retry_rclone(&mut self) {
        self.rclone_error = None;
        self.spawn_rclone();
    }

    fn rclone_ready(&mut self, res: Result<Rcd, String>) {
        self.rclone_starting = false;
        match res {
            Ok(rcd) => {
                self.client = Some(rcd.client.clone());
                self.caps = rcd.caps;
                self.rcd = Some(rcd);
                self.rclone_error = None;
                self.poll_failures = 0;
                self.apply_options();
                for i in 0..self.cfg.accounts.len() {
                    self.mount(i);
                }
            }
            Err(e) => {
                eprintln!("nuvolo: {e}");
                self.rclone_error = Some(e);
            }
        }
    }

    /// Unmounts everything and stops rclone.
    pub fn shutdown(&mut self) {
        self.stopped = true;
        self.rcd.take(); // unmounts what it can, then rclone exits
        // A mount that was busy (open in Files) is dead now that rclone is gone. Detach it, or
        // the next start finds "Transport endpoint is not connected" (issue #8).
        let mounted = rclone::mounted_paths();
        for a in &self.cfg.accounts {
            if rclone::is_mounted_path(&mounted, &a.mount) {
                rclone::lazy_unmount(&rclone::canonical_mount_path(&a.mount));
            }
        }
    }

    /// Restarts rclone in the background if it died (or never started).
    pub(super) fn ensure_rclone(&mut self) {
        if self.stopped || self.rclone_starting || self.rcd.as_mut().is_some_and(|r| r.alive()) {
            return;
        }
        if self.ticks % 5 != 0 {
            return; // retry every 5 s while rclone will not start
        }
        eprintln!("nuvolo: rclone is not running, starting it again");
        self.spawn_rclone();
    }

    /// Handles everything background work has reported since the last call, ignoring stale
    /// results from a previous rclone instance.
    pub(super) fn drain_done(&mut self) {
        while let Ok(d) = self.done_rx.try_recv() {
            match d {
                Done::RcloneReady(gen, res) if gen == self.rclone_gen => self.rclone_ready(res),
                Done::Mounted(gen, i, res, held) if gen == self.rclone_gen => self.mounted(i, res, held),
                Done::Unmounted(gen, i, res, kind) if gen == self.rclone_gen => self.unmounted(i, res, kind),
                Done::Synced(gen, i, res, bytes) if gen == self.rclone_gen => self.synced(i, res, bytes),
                Done::Refreshed(gen, i) if gen == self.rclone_gen => self.sync_offline(i),
                Done::Paused(gen, on, res) if gen == self.rclone_gen => match res {
                    Ok(()) => self.paused = on,
                    Err(e) => eprintln!("nuvolo: pause: {e}"),
                },
                _ => {} // stale result from an earlier rclone generation
            }
        }
    }

    /// True when transfers, uploads, mounts, offline syncs, home moves, or rclone startup are active.
    pub fn is_active(&self) -> bool {
        self.rclone_starting
            || !self.mounting.is_empty()
            || !self.unmounting.is_empty()
            || !self.offline_running.is_empty()
            || !self.jobs.is_empty()
            || !self.syncing.is_empty()
            || self.vfs.values().any(|v| uploads_in(v) > 0)
    }

    pub fn save(&self) {
        if let Err(e) = config::save(&self.cfg) {
            eprintln!("nuvolo: could not save {}: {e}", config::path().display());
        }
    }

    pub fn apply_options(&self) {
        let Some(client) = self.client.clone() else { return };
        let opts = json!({
            "main": {
                "Transfers": self.cfg.transfers,
                "Checkers": self.cfg.checkers,
                "BufferSize": 33554432u64, // 32 MiB
            }
        });
        std::thread::spawn(move || {
            if let Err(e) = client.call_local("options/set", opts) {
                eprintln!("nuvolo: options/set: {e}");
            }
        });
    }

    pub fn add_account(&mut self, remote: &str) -> usize {
        self.cfg.accounts.push(Account::new(remote));
        self.save();
        let i = self.cfg.accounts.len() - 1;
        self.mount(i);
        i
    }

    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// The account whose mount folder holds `abs`, and `abs` relative to it.
    pub fn locate(&self, abs: &Path) -> Option<(usize, String)> {
        let abs_canon = rclone::canonical_mount_path(abs);
        self.cfg.accounts.iter().enumerate().find_map(|(i, a)| {
            abs.strip_prefix(&a.mount)
                .or_else(|_| abs_canon.strip_prefix(rclone::canonical_mount_path(&a.mount)))
                .ok()
                .map(|r| (i, r.to_string_lossy().into_owned()))
        })
    }

    // ponytail: rclone has no global pause, so pausing throttles bandwidth to 1 byte/s.
    pub fn set_paused(&mut self, on: bool) -> Result<(), String> {
        let Some(client) = self.client.clone() else {
            return Err("rclone is not running yet".into());
        };
        let gen = self.rclone_gen;
        let tx = self.done_tx.clone();
        self.paused = on; // optimistic update; reverted/logged if rc call fails
        std::thread::spawn(move || {
            let res = client.call_local("core/bwlimit", json!({ "rate": if on { "1B" } else { "off" } })).map(|_| ());
            let _ = tx.send(Done::Paused(gen, on, res));
        });
        Ok(())
    }

    /// What a link lookup needs, so the network call can run off the main thread.
    pub fn link_job(&self, abs: &Path) -> Option<(Client, String, String)> {
        let client = self.client.clone()?;
        let (i, rel) = self.locate(abs)?;
        Some((client, self.cfg.accounts[i].remote.clone(), rel))
    }
}
