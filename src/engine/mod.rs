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

pub use mounts::MountState;
pub use offline::OfflineProblem;
pub use poll::uploads_in;

use crate::config::{self, Account, Config};
use crate::rclone::{self, Client, Rcd};
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

pub type Shared = Rc<RefCell<Engine>>;

/// A result from background work, sent back to the main thread.
pub enum Done {
    /// Mounting account `i` finished.
    Mounted(usize, Result<(), String>),
    /// Syncing the offline copies of account `i` finished; with their size on disk.
    Synced(usize, Result<(), OfflineProblem>, u64),
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
    pub client: Client,
    rcd: Option<Rcd>,
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
    last_count: Option<u64>,
    last_bytes: Option<(f64, std::time::Instant)>,
    syncing: HashSet<PathBuf>,
    errors: HashSet<PathBuf>,
    pub paused: bool,

    // Mounts, from `mounts`.
    /// Why an account is not mounted, by remote.
    mount_errors: HashMap<String, String>,
    /// Accounts being mounted now.
    mounting: HashSet<usize>,
    /// Accounts whose remount was deferred because uploads were pending.
    remount_later: HashSet<usize>,

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
    /// Starts rclone and begins mounting every account. Returns at once: mounts finish in
    /// the background, so Nuvolo starts even without internet.
    pub fn start() -> Result<Shared, String> {
        let cfg = config::load()?;
        let cache_dir = glib::user_cache_dir().join("nuvolo");
        let rcd = Rcd::spawn(cfg.rc_port, &cache_dir).map_err(|e| format!("could not start rclone: {e}"))?;
        let (done_tx, done_rx) = channel();
        let mut e = Engine {
            client: rcd.client.clone(),
            rcd: Some(rcd),
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
            remount_later: HashSet::new(),
            offline_dirs: HashMap::new(),
            offline_running: HashSet::new(),
            offline_errors: HashMap::new(),
            offline_bytes: HashMap::new(),
            jobs: HashMap::new(),
        };
        e.apply_options();
        for i in 0..e.cfg.accounts.len() {
            e.mount(i);
        }
        Ok(Rc::new(RefCell::new(e)))
    }

    /// Unmounts everything and stops rclone.
    pub fn shutdown(&mut self) {
        self.stopped = true;
        self.rcd.take(); // unmounts what it can, then rclone exits
        // A mount that was busy (open in Files) is dead now that rclone is gone. Detach it, or
        // the next start finds "Transport endpoint is not connected" (issue #8).
        let mounted = rclone::mounted_paths();
        for a in &self.cfg.accounts {
            if mounted.contains(&a.mount) {
                rclone::lazy_unmount(&a.mount);
            }
        }
    }

    /// Restarts rclone if it died, then mounts everything again.
    fn ensure_rclone(&mut self) {
        if self.stopped || self.rcd.as_mut().is_some_and(|r| r.alive()) {
            return;
        }
        if self.ticks % 5 != 0 {
            return; // retry every 5 s while rclone will not start
        }
        eprintln!("nuvolo: rclone is not running, starting it again");
        self.rcd.take();
        match Rcd::spawn(self.cfg.rc_port, &self.cache_dir) {
            Ok(rcd) => {
                self.client = rcd.client.clone();
                self.rcd = Some(rcd);
                self.paused = false;
                self.mounting.clear(); // those threads talked to the old rclone
                self.apply_options();
                for i in 0..self.cfg.accounts.len() {
                    self.mount(i);
                }
            }
            Err(e) => eprintln!("nuvolo: could not restart rclone: {e}"),
        }
    }

    /// Handles everything background work has reported since the last call.
    fn drain_done(&mut self) {
        while let Ok(d) = self.done_rx.try_recv() {
            match d {
                Done::Mounted(i, res) => self.mounted(i, res),
                Done::Synced(i, res, bytes) => self.synced(i, res, bytes),
            }
        }
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
        self.cfg
            .accounts
            .iter()
            .enumerate()
            .find_map(|(i, a)| abs.strip_prefix(&a.mount).ok().map(|r| (i, r.to_string_lossy().into_owned())))
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
