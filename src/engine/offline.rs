//! Offline copies: pinned (Always Offline) folders and files also exist as real files in the
//! mount folder itself, underneath the mount.
//!
//! While the drive is mounted, the mount covers them and `~/Cloud/<remote>` shows the live
//! drive. When rclover is not running, or the drive could not be mounted (no internet), the
//! same paths show the offline copies. So links into pinned folders (home folders, dotfiles
//! in ~/.config) keep working at all times.
//!
//! How the covered folder stays reachable: before mounting, rclover opens the mount folder and
//! keeps it open. Paths through that handle (`/proc/<pid>/fd/<n>/...`) lead under the mount,
//! not into it. A stable symlink, `~/.cache/rclover/offline/<remote>`, points there, so
//! rclone's two-way sync state survives restarts.
//!
//! While the drive is mounted, nothing but rclover writes to the offline copies: every link
//! leads into the mount. So after each mount, each copy is synced both ways once
//! ([`rclone::bisync`]), which uploads what changed while the drive was not mounted. After
//! that, until the drive is unmounted, the copies only follow the cloud ([`rclone::mirror`]),
//! after each change of mode and every 10 minutes. Edits made through the mount are being
//! uploaded at the same time; a two-way sync reading the cloud then stopped with "out of
//! sync" and started afresh, which brought deleted files back. A one-way copy cannot conflict
//! and fixes a half-seen change on its next run. Each sync first waits for the mount's
//! uploads to finish.
//!
//! Anything else found under the mount (written while it was not mounted, or a copy that is
//! no longer pinned) is moved into the drive first, so it gets uploaded instead of staying
//! hidden under the mount. Files the cloud already has, byte for byte, are not copied again.
//!
//! A pinned folder renamed or moved in the cloud (through the mount, or elsewhere) is followed
//! by its cloud ID: its offline copy is renamed and the pin, with the modes inside it, moves to
//! the new path. One deleted in the cloud has its offline copy moved to the Trash. Either way the
//! old copy is never mistaken for loose files and uploaded again under the old name.
//!
//! Pinned files are not loaded into rclone's VFS cache: the offline copies are the fixed
//! baseline of local storage, and the VFS cache limit is only the space for on-demand files.
// ponytail: bisync's record of a folder dates from the first sync after a mount; mirrors do not
// update it. A file created in the cloud during that session and then deleted here while not
// mounted comes back at the next mount (nothing is lost). Upgrade path: rewrite bisync's
// listings after each mirror, once their format is stable API.
// ponytail: while mounted, opening a pinned file reads it through the mount (from the network,
// into the VFS cache), not from its offline copy; if the network drops mid-session, only the
// offline copies under the mount have it. Upgrade path: unmount when the network goes away, so
// the offline copies show.

use super::{Done, Engine, SyncReport};
use crate::config::{Account, Config, ConflictPolicy, Mode};
use crate::errors::explain;
use crate::homedirs::{merge_entry_with, CopyPolicy};
use crate::notify;
use crate::rclone::{self, Client, Scope};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

impl Engine {
    pub(super) fn offline_link(&self, remote: &str) -> PathBuf {
        self.cache_dir.join("offline").join(remote)
    }

    /// Why the offline copies of `remote` are not up to date, if they are not.
    pub fn offline_error(&self, remote: &str) -> Option<&OfflineProblem> {
        self.offline_errors.get(remote)
    }

    /// Space the offline copies of `remote` take, as of their last sync.
    pub fn offline_bytes(&self, remote: &str) -> Option<u64> {
        self.offline_bytes.get(remote).copied()
    }

    /// True while the offline copies of account `i` are syncing.
    pub fn offline_syncing(&self, i: usize) -> bool {
        self.offline_running.contains(&i)
    }

    /// Syncs the offline copies of account `i` with the cloud, in the background. Does
    /// nothing unless the drive is mounted and the folder under it is reachable.
    pub fn sync_offline(&mut self, i: usize) {
        self.start_offline_sync(i, false);
    }

    /// "Sync Anyway": syncs past bisync's safety stops (mass deletion, every file changed).
    pub fn sync_offline_anyway(&mut self, i: usize) {
        self.start_offline_sync(i, true);
    }

    fn start_offline_sync(&mut self, i: usize, force: bool) {
        if !self.offline_dirs.contains_key(&i) || self.offline_running.contains(&i) || !self.is_mounted(i) {
            return;
        }
        let Some(client) = self.client.clone() else { return };
        let a = self.cfg.accounts[i].clone();
        let pinned = pinned_roots(&a);
        let excludes = pinned.iter().map(|rel| excludes_below(&self.cfg, &a, rel)).collect();
        let excluded = excluded_roots(&a);
        let job = OfflineJob {
            client,
            pushed: self.offline_pushed.get(&i).cloned().unwrap_or_default(),
            under: self.offline_link(&a.remote),
            workdir: self.cache_dir.join("bisync"),
            ids: self.cache_dir.join(format!("ids-{}.json", a.remote)),
            trash: gtk::glib::user_data_dir().join("Trash"),
            pinned,
            excluded,
            excludes,
            policy: self.cfg.conflicts,
            force,
            account: a,
        };
        self.offline_running.insert(i);
        let gen = self.rclone_gen;
        let epoch = self.mount_epoch.get(&i).copied().unwrap_or(0);
        let tx = self.done_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done::Synced(gen, i, SyncReport { epoch, ..job.run() }));
        });
    }

    /// An offline sync finished.
    pub(super) fn synced(&mut self, i: usize, r: SyncReport) {
        self.offline_running.remove(&i);
        // Only for the mount it ran under: after a remount, offline edits may be waiting again.
        if self.mount_epoch.get(&i).copied().unwrap_or(0) == r.epoch {
            self.offline_pushed.entry(i).or_default().extend(r.pushed);
        }
        let (res, bytes) = (r.res, r.bytes);
        let Some(remote) = self.cfg.accounts.get(i).map(|a| a.remote.clone()) else { return };
        if !r.moved.is_empty() || !r.gone.is_empty() {
            let a = &mut self.cfg.accounts[i];
            for (old, new) in &r.moved {
                a.move_modes(old, new);
                notify::notify(&format!("moved-{remote}-{old}"), &format!("“{old}” is now “{new}”"), "It was renamed or moved in the cloud, and stays on this device under its new name.");
            }
            for rel in &r.gone {
                a.forget(rel);
                notify::notify(&format!("gone-{remote}-{rel}"), &format!("“{rel}” was deleted in the cloud"), "Its offline copy is in the Trash. Restore it from there to put it back in the drive.");
            }
            self.save();
        }
        self.offline_bytes.insert(remote.clone(), bytes);
        let id = format!("offline-{remote}");
        match res {
            Ok(()) => {
                if self.offline_errors.remove(&remote).is_some() {
                    notify::withdraw(&id);
                }
            }
            Err(p) => {
                if self.offline_errors.get(&remote) != Some(&p) {
                    notify::notify(&id, &format!("Offline copies of “{remote}” are not up to date"), &p.text);
                }
                self.offline_errors.insert(remote, p);
            }
        }
    }
}

/// Why offline copies are not up to date, in words for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfflineProblem {
    /// Which folder, what happened, what to do.
    pub text: String,
    /// A safety stop that "Sync Anyway" overrides.
    pub can_force: bool,
}

/// Everything one offline sync needs, moved to its thread.
struct OfflineJob {
    client: Client,
    /// Sync units ([`unit_key`]) synced both ways since the drive was mounted: they mirror.
    pushed: HashSet<String>,
    account: Account,
    /// The folder under the mount (through the held handle).
    under: PathBuf,
    workdir: PathBuf,
    /// Cloud IDs of the pinned paths, as last seen (`ids-<remote>.json` in the cache).
    ids: PathBuf,
    /// The user's Trash (`~/.local/share/Trash`), for offline copies of deleted folders.
    trash: PathBuf,
    pinned: Vec<String>,
    /// Excluded roots, skipped by stray rescue.
    excluded: Vec<String>,
    /// Exclude rules for each entry of `pinned`.
    excludes: Vec<Vec<String>>,
    policy: ConflictPolicy,
    force: bool,
}

impl OfflineJob {
    /// Waits for uploads, follows pinned folders renamed in the cloud, moves loose files into
    /// the drive, then syncs each pinned folder, then the pinned single files (grouped by
    /// folder). Goes on past a failing folder, and reports the first failure. The report's
    /// `epoch` is left for the caller.
    fn run(&self) -> SyncReport {
        let mount = &self.account.mount;
        let fail = |what: &str, raw: String| {
            eprintln!("rclover: offline copy of {what}: {raw}");
            OfflineProblem {
                text: format!("“{what}”: {}", explain(&raw)),
                can_force: raw.contains("too many deletes") || raw.contains("all files were changed"),
            }
        };
        let mut first: Option<OfflineProblem> = None;
        let fs_ = self.account.fs();
        rclone::wait_for_uploads(&self.client, &fs_, Duration::from_secs(600));
        let prune = |local: &Path, rel: &str| prune_identical(&self.client, local, &format!("{fs_}{rel}"));
        let followed = self.follow_renames(&fs_);
        if let Some((what, e)) = followed.problem {
            first = Some(fail(&what, e));
        }
        let kept: Vec<String> = followed.pinned.iter().chain(&followed.held).cloned().collect();
        if let Err(e) = rescue_strays(&self.under, mount, &kept, &self.excluded, &prune) {
            first = Some(fail("files saved while the drive was not mounted", e.to_string()));
        }
        let tidy = self.policy == ConflictPolicy::KeepBoth;
        let mut total_bytes: u64 = 0;
        let mut pushed_now = Vec::new();
        let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (rel, excludes) in followed.pinned.iter().zip(&followed.excludes) {
            let local = if rel.is_empty() { self.under.clone() } else { self.under.join(rel) };
            // A pinned path is a file if either side says so (the cloud side, through the mount).
            if local.is_file() || mount.join(rel).is_file() {
                let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
                files.entry(dir.to_owned()).or_default().push(name.to_owned());
                continue;
            }
            let key = unit_key(false, rel);
            let two_way = !self.pushed.contains(&key);
            let remote = format!("{fs_}{rel}");
            let scope = Scope::All(excludes);
            let res = fs::create_dir_all(&local).map_err(|e| e.to_string()).and_then(|_| {
                if !two_way {
                    return rclone::mirror(&self.client, &local, &remote, scope, self.force);
                }
                rclone::bisync(&self.client, &local, &remote, &self.workdir, scope, self.policy, self.force)?;
                // Renamed conflict copies exist only here: upload them now, before a mirror
                // would delete them as gone from the cloud.
                if tidy && tidy_conflicts(&local).map_err(|e| e.to_string())? > 0 {
                    rclone::bisync(&self.client, &local, &remote, &self.workdir, scope, self.policy, false)?;
                }
                Ok(())
            });
            match res {
                Ok(()) if two_way => pushed_now.push(key),
                Ok(()) => {}
                Err(e) => {
                    first.get_or_insert(fail(if rel.is_empty() { "whole drive" } else { rel }, e));
                }
            }
            total_bytes += du(&local);
        }
        for (dir, names) in files {
            let local = if dir.is_empty() { self.under.clone() } else { self.under.join(&dir) };
            // Single files keep bisync's own conflict name ("app.toml.conflict1"): a readable
            // name would fall outside the files this sync covers.
            let key = unit_key(true, &dir);
            let two_way = !self.pushed.contains(&key);
            let remote = format!("{fs_}{dir}");
            let res = fs::create_dir_all(&local).map_err(|e| e.to_string()).and_then(|_| {
                if two_way {
                    rclone::bisync(&self.client, &local, &remote, &self.workdir, Scope::Files(&names), self.policy, true)
                } else {
                    rclone::mirror(&self.client, &local, &remote, Scope::Files(&names), true)
                }
            });
            match res {
                Ok(()) if two_way => pushed_now.push(key),
                Ok(()) => {}
                Err(e) => {
                    first.get_or_insert(fail(&names[0], e));
                }
            }
            for name in &names {
                total_bytes += du(&local.join(name));
            }
        }
        SyncReport { epoch: 0, pushed: pushed_now, res: first.map_or(Ok(()), Err), bytes: total_bytes, moved: followed.moved, gone: followed.gone }
    }

    /// Checks each pinned path's cloud ID against the one last seen, and follows a rename,
    /// move or deletion (see [`follow`]). Without a recorded ID nothing changes.
    fn follow_renames(&self, fs_: &str) -> Followed {
        let mut ids: BTreeMap<String, String> = fs::read(&self.ids).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let before = ids.clone();
        let mut f = Followed::default();
        for (rel, excludes) in self.pinned.iter().zip(&self.excludes) {
            let action = if rel.is_empty() {
                Follow::Keep // the whole drive
            } else {
                follow(ids.get(rel).map(String::as_str), rclone::id_at(&self.client, fs_, rel), |id| rclone::find_id(&self.client, fs_, rel, id))
            };
            let mut problem = |e: io::Error| {
                f.problem.get_or_insert((rel.clone(), e.to_string()));
                f.held.push(rel.clone());
            };
            match action {
                Follow::Keep => {}
                Follow::Record(id) => {
                    ids.insert(rel.clone(), id);
                }
                Follow::Unknown => {
                    f.held.push(rel.clone()); // left as it is until the cloud can be asked again
                    continue;
                }
                Follow::Moved(new) => {
                    match move_copy(&self.under, rel, &new) {
                        Ok(()) => {
                            eprintln!("rclover: “{rel}” is now “{new}” in the cloud; its offline copy follows");
                            let id = ids.remove(rel).unwrap_or_default();
                            ids.insert(new.clone(), id);
                            f.moved.push((rel.clone(), new.clone()));
                            f.pinned.push(new);
                            f.excludes.push(excludes.clone());
                        }
                        Err(e) => problem(e),
                    }
                    continue;
                }
                Follow::Deleted { replaced } => {
                    match trash_copy(&self.under, &self.account.mount, rel, &self.trash) {
                        Ok(()) => {
                            eprintln!("rclover: “{rel}” was deleted in the cloud; its offline copy is in the Trash");
                            ids.remove(rel);
                            if !replaced {
                                f.gone.push(rel.clone());
                                continue;
                            }
                        }
                        Err(e) => {
                            problem(e);
                            continue;
                        }
                    }
                }
            }
            f.pinned.push(rel.clone());
            f.excludes.push(excludes.clone());
        }
        ids.retain(|k, _| f.pinned.contains(k));
        if ids != before {
            if let Err(e) = serde_json::to_vec(&ids).map_err(io::Error::other).and_then(|b| fs::write(&self.ids, b)) {
                eprintln!("rclover: could not save {}: {e}", self.ids.display());
            }
        }
        f
    }
}

/// Pinned paths after following renames, for one offline sync.
#[derive(Default)]
struct Followed {
    /// Pinned roots to sync, and their exclude rules.
    pinned: Vec<String>,
    excludes: Vec<Vec<String>>,
    /// Offline copies left alone this time; never treated as loose files.
    held: Vec<String>,
    moved: Vec<(String, String)>,
    gone: Vec<String>,
    /// Which pinned path, and what went wrong.
    problem: Option<(String, String)>,
}

/// What became of a pinned path in the cloud.
#[derive(Debug, PartialEq, Eq)]
enum Follow {
    /// As before (or nothing to compare with).
    Keep,
    /// Still there; remember this ID.
    Record(String),
    /// The cloud could not be asked.
    Unknown,
    /// Renamed or moved to this path.
    Moved(String),
    /// Deleted in the cloud; `replaced` if something else now has its path.
    Deleted { replaced: bool },
}

/// Decides from `recorded`, the ID last seen at the path, and `at`, the ID there now.
/// `find(id)` searches the cloud for where that ID went. With no recorded ID, a missing path is
/// new on this device (made while offline) and is uploaded as usual.
fn follow(recorded: Option<&str>, at: Result<Option<String>, String>, find: impl FnOnce(&str) -> Result<Option<String>, String>) -> Follow {
    let Ok(at) = at else { return Follow::Keep };
    match (at, recorded) {
        (Some(id), r) if id.is_empty() || r.is_none_or(|r| r == id) => {
            if id.is_empty() || r == Some(id.as_str()) {
                Follow::Keep
            } else {
                Follow::Record(id)
            }
        }
        (_, None) => Follow::Keep,
        (at, Some(old)) => match find(old) {
            Err(_) => Follow::Unknown,
            Ok(Some(new)) => Follow::Moved(new),
            Ok(None) => Follow::Deleted { replaced: at.is_some() },
        },
    }
}

/// Renames the offline copy of `old` to `new` (both relative to `under`).
fn move_copy(under: &Path, old: &str, new: &str) -> io::Result<()> {
    let (from, to) = (under.join(old), under.join(new));
    if from.symlink_metadata().is_err() {
        return Ok(()); // no copy yet
    }
    if to.symlink_metadata().is_ok() {
        return Err(io::Error::other(format!("it is now “{new}” in the cloud, but something is already there on this device")));
    }
    fs::create_dir_all(to.parent().expect("inside under"))?;
    fs::rename(from, to)
}

/// Moves the offline copy of `rel` to the Trash (freedesktop.org Trash spec), recorded as
/// coming from its path in the drive, so restoring it puts it back into the cloud.
fn trash_copy(under: &Path, mount: &Path, rel: &str, trash: &Path) -> io::Result<()> {
    let from = under.join(rel);
    if from.symlink_metadata().is_err() {
        return Ok(());
    }
    let (files, info) = (trash.join("files"), trash.join("info"));
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let name = (1..)
        .map(|n| if n == 1 { base.to_owned() } else { format!("{base}.{n}") })
        .find(|n| files.join(n).symlink_metadata().is_err() && info.join(format!("{n}.trashinfo")).symlink_metadata().is_err())
        .expect("a free name exists");
    let date = gtk::glib::DateTime::now_local().ok().and_then(|d| d.format("%Y-%m-%dT%H:%M:%S").ok()).map(|s| s.to_string()).unwrap_or_default();
    let path = gtk::glib::Uri::escape_string(&mount.join(rel).to_string_lossy(), Some("/"), false);
    let info_file = info.join(format!("{name}.trashinfo"));
    fs::write(&info_file, format!("[Trash Info]\nPath={path}\nDeletionDate={date}\n"))?;
    fs::rename(&from, files.join(&name)).inspect_err(|_| {
        let _ = fs::remove_file(&info_file);
    })
}

/// Names one sync unit: a pinned folder, or the pinned single files of one folder.
fn unit_key(files: bool, rel: &str) -> String {
    format!("{}:{rel}", if files { "files" } else { "dir" })
}

/// Deletes the files in `local` (a file, or a folder) that the cloud at `remote` already has
/// with the same MD5, so moving the rest into the drive copies only what is new. On any error
/// it deletes nothing: everything is then copied, as before.
fn prune_identical(c: &Client, local: &Path, remote: &str) {
    let sums = |fs_: &str| -> Option<HashMap<String, String>> {
        let v = c.call_for("operations/hashsum", json!({ "fs": fs_, "hashType": "md5" }), Duration::from_secs(3600)).ok()?;
        Some(v["hashsum"].as_array()?.iter().filter_map(|l| l.as_str()?.split_once("  ")).filter(|(h, _)| !h.is_empty()).map(|(h, n)| (n.to_owned(), h.to_owned())).collect())
    };
    let (Some(here), Some(there)) = (sums(&local.to_string_lossy()), sums(remote)) else { return };
    let is_file = local.is_file();
    for (name, hash) in &here {
        if there.get(name) == Some(hash) {
            let _ = fs::remove_file(if is_file { local.to_path_buf() } else { local.join(name) });
        }
    }
}

/// Gives conflict copies a readable name that keeps the extension, so they open as usual:
/// bisync's "notes.txt.conflict1" becomes "notes (conflict 1).txt". Returns how many were
/// renamed; a two-way sync must then pass the renames on to the cloud.
fn tidy_conflicts(dir: &Path) -> io::Result<usize> {
    let mut renamed = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            renamed += tidy_conflicts(&path)?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((base, n)) = name.rsplit_once(".conflict").filter(|(b, n)| !b.is_empty() && !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())) else { continue };
        let (stem, ext) = match base.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
            _ => (base, String::new()),
        };
        let target = (0..)
            .map(|k| dir.join(if k == 0 { format!("{stem} (conflict {n}){ext}") } else { format!("{stem} (conflict {n}-{k}){ext}") }))
            .find(|t| t.symlink_metadata().is_err())
            .expect("a free name exists");
        fs::rename(&path, target)?;
        renamed += 1;
    }
    Ok(renamed)
}

/// Bytes of the files under `p` (or of `p` itself), not following links.
fn du(p: &Path) -> u64 {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::read_dir(p).into_iter().flatten().flatten().map(|e| du(&e.path())).sum(),
        Ok(m) if m.is_file() => m.len(),
        _ => 0,
    }
}

/// Pinned paths, without those inside another pinned path (the outer one covers them).
fn pinned_roots(a: &Account) -> Vec<String> {
    let pinned: Vec<&String> = a.modes.iter().filter(|(_, m)| **m == Mode::Pinned).map(|(p, _)| p).collect();
    pinned
        .iter()
        .filter(|p| !pinned.iter().any(|q| q != *p && inside(p, q)))
        .map(|p| (*p).clone())
        .collect()
}

/// Excluded paths, without those inside another excluded path.
fn excluded_roots(a: &Account) -> Vec<String> {
    let excluded: Vec<&String> = a.modes.iter().filter(|(_, m)| **m == Mode::Excluded).map(|(p, _)| p).collect();
    excluded
        .iter()
        .filter(|p| !excluded.iter().any(|q| q != *p && inside(p, q)))
        .map(|p| (*p).clone())
        .collect()
}

/// True if `rel` is `root` or inside it ("" is the whole drive).
fn inside(rel: &str, root: &str) -> bool {
    root.is_empty() || rel == root || rel.starts_with(&format!("{root}/"))
}

/// Exclude rules for syncing `root`: the ignore patterns, and excluded folders inside it,
/// anchored at `root` as the sync sees it.
fn excludes_below(cfg: &Config, a: &Account, root: &str) -> Vec<String> {
    let mut rules = cfg.ignore.clone();
    for (p, m) in &a.modes {
        if *m == Mode::Excluded && inside(p, root) && p != root {
            let below = if root.is_empty() { p.as_str() } else { &p[root.len() + 1..] };
            rules.push(format!("/{below}/**"));
        }
    }
    rules
}

/// Moves into the mount whatever lies under it outside the pinned and excluded paths, so it is
/// uploaded. Always copies (never skips on equal size) so deleting the local stray afterwards
/// can never lose a different same-size file. Returns how many entries were moved.
///
/// `prune(local, rel)` runs on each entry before it is moved, to drop files the cloud has.
fn rescue_strays(under: &Path, mount: &Path, pinned: &[String], excluded: &[String], prune: &dyn Fn(&Path, &str)) -> io::Result<usize> {
    // Never copy into the folder itself: that is what `mount` is when the mount is not up,
    // and removing the "copied" original would then lose it.
    let dev = |p: &Path| fs::metadata(p).map(|m| m.dev());
    if dev(under)? == dev(mount)? {
        return Err(io::Error::other("the drive is not mounted"));
    }
    walk_strays(under, mount, "", pinned, excluded, prune)
}

fn walk_strays(under: &Path, mount: &Path, dir: &str, pinned: &[String], excluded: &[String], prune: &dyn Fn(&Path, &str)) -> io::Result<usize> {
    let mut moved = 0;
    for entry in fs::read_dir(under.join(dir))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if dir.is_empty() { name } else { format!("{dir}/{name}") };
        if pinned.iter().any(|p| inside(&rel, p)) || excluded.iter().any(|ex| inside(&rel, ex)) {
            continue; // an offline copy or excluded folder
        }
        let is_dir = entry.file_type()?.is_dir();
        let prefix = format!("{rel}/");
        if is_dir && (pinned.iter().any(|p| p.starts_with(&prefix)) || excluded.iter().any(|ex| ex.starts_with(&prefix))) {
            moved += walk_strays(under, mount, &rel, pinned, excluded, prune)?; // holds a pinned or excluded path deeper down
            continue;
        }
        prune(&entry.path(), &rel);
        if !is_dir && entry.path().symlink_metadata().is_err() {
            continue; // the cloud has it already
        }
        let to = mount.join(dir);
        fs::create_dir_all(&to)?;
        merge_entry_with(&entry, &to, &AtomicUsize::new(0), CopyPolicy::AlwaysCopy)?;
        if is_dir {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
        moved += 1;
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_renames_and_deletions_by_cloud_id() {
        let found = |p: &str| { let p = p.to_owned(); move |_: &str| Ok(Some(p)) };
        let not_found = |_: &str| Ok(None);
        let no_search = |_: &str| -> Result<Option<String>, String> { panic!("no search needed") };
        // First sight records the ID; same ID again changes nothing.
        assert_eq!(follow(None, Ok(Some("A".into())), no_search), Follow::Record("A".into()));
        assert_eq!(follow(Some("A"), Ok(Some("A".into())), no_search), Follow::Keep);
        // A cloud without IDs, or the cloud not answering: as before.
        assert_eq!(follow(Some("A"), Ok(Some(String::new())), no_search), Follow::Keep);
        assert_eq!(follow(Some("A"), Err("offline".into()), no_search), Follow::Keep);
        // Never seen in the cloud: made on this device while offline, uploaded as usual.
        assert_eq!(follow(None, Ok(None), no_search), Follow::Keep);
        // Gone from its path: renamed or moved, or deleted.
        assert_eq!(follow(Some("A"), Ok(None), found("Projects/rclover")), Follow::Moved("Projects/rclover".into()));
        assert_eq!(follow(Some("A"), Ok(None), not_found), Follow::Deleted { replaced: false });
        assert_eq!(follow(Some("A"), Ok(None), |_| Err("timeout".into())), Follow::Unknown);
        // Another folder took its name: the pinned one is followed by ID all the same.
        assert_eq!(follow(Some("A"), Ok(Some("B".into())), found("Old/nuvolo")), Follow::Moved("Old/nuvolo".into()));
        assert_eq!(follow(Some("A"), Ok(Some("B".into())), not_found), Follow::Deleted { replaced: true });
    }

    #[test]
    fn a_renamed_or_deleted_folder_is_not_uploaded_again() {
        let root = std::env::temp_dir().join(format!("rclover-follow-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (under, mount, trash) = (root.join("under"), root.join("mount"), root.join("Trash"));
        fs::create_dir_all(under.join("Projects/nuvolo/src")).unwrap();
        fs::write(under.join("Projects/nuvolo/src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(under.join("Old")).unwrap();
        fs::write(under.join("Old/notes.txt"), "keep me").unwrap();
        fs::create_dir_all(&mount).unwrap();

        move_copy(&under, "Projects/nuvolo", "Projects/rclover").unwrap();
        assert_eq!(fs::read_to_string(under.join("Projects/rclover/src/main.rs")).unwrap(), "fn main() {}");
        assert!(!under.join("Projects/nuvolo").exists());
        assert!(move_copy(&under, "Old", "Projects/rclover").is_err(), "never merged into something already there");

        trash_copy(&under, &mount, "Old", &trash).unwrap();
        assert!(!under.join("Old").exists());
        assert_eq!(fs::read_to_string(trash.join("files/Old/notes.txt")).unwrap(), "keep me");
        let info = fs::read_to_string(trash.join("info/Old.trashinfo")).unwrap();
        assert!(info.contains(&format!("Path={}/Old\n", mount.display())), "{info}");
        fs::create_dir_all(under.join("Old")).unwrap();
        trash_copy(&under, &mount, "Old", &trash).unwrap();
        assert!(trash.join("files/Old.2").is_dir(), "a second one gets a free name");
        assert!(move_copy(&under, "Missing", "Elsewhere").is_ok() && trash_copy(&under, &mount, "Missing", &trash).is_ok(), "no copy, nothing to do");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_pins_and_excludes_are_relative_to_the_synced_folder() {
        let mut a = Account::new("t");
        a.modes.insert("Projects".into(), Mode::Pinned);
        a.modes.insert("Projects/web".into(), Mode::Pinned);
        a.modes.insert("Projects/web/node_modules".into(), Mode::Excluded);
        a.modes.insert(".config/app.toml".into(), Mode::Pinned);
        assert_eq!(pinned_roots(&a), vec![".config/app.toml".to_owned(), "Projects".to_owned()]);
        let cfg = Config { ignore: vec!["*.tmp".into()], ..Config::default() };
        assert_eq!(excludes_below(&cfg, &a, "Projects"), vec!["*.tmp".to_owned(), "/web/node_modules/**".to_owned()]);
        assert_eq!(excludes_below(&cfg, &a, ""), vec!["*.tmp".to_owned(), "/Projects/web/node_modules/**".to_owned()]);
    }

    #[test]
    fn conflict_copies_get_readable_names() {
        let d = std::env::temp_dir().join(format!("rclover-conflicts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sub")).unwrap();
        for f in ["notes.txt.conflict1", "sub/Makefile.conflict2", "photo.conflict.jpg", "plain.txt"] {
            fs::write(d.join(f), f).unwrap();
        }
        assert_eq!(tidy_conflicts(&d).unwrap(), 2);
        assert_eq!(fs::read_to_string(d.join("notes (conflict 1).txt")).unwrap(), "notes.txt.conflict1");
        assert!(d.join("sub/Makefile (conflict 2)").exists());
        assert!(d.join("photo.conflict.jpg").exists(), "not a conflict copy");
        assert!(d.join("plain.txt").exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn strays_refuse_to_move_onto_the_same_disk() {
        // Without a mount, "under" and "mount" are the same folder: nothing may be moved.
        let d = std::env::temp_dir().join(format!("rclover-strays-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("loose")).unwrap();
        fs::write(d.join("loose/a.txt"), "keep me").unwrap();
        assert!(rescue_strays(&d, &d, &[], &[], &|_, _| {}).is_err());
        assert_eq!(fs::read_to_string(d.join("loose/a.txt")).unwrap(), "keep me");
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn stray_rescue_keeps_same_size_different_file() {
        let root = std::env::temp_dir().join(format!("rclover-stray-same-size-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (under, mount) = (root.join("under"), root.join("mount"));
        fs::create_dir_all(&under).unwrap();
        fs::create_dir_all(&mount).unwrap();
        // Same name, same 4-byte length, different contents.
        fs::write(mount.join("a.txt"), "aaaa").unwrap();
        fs::write(under.join("a.txt"), "bbbb").unwrap();
        let moved = walk_strays(&under, &mount, "", &[], &[], &|_, _| {}).unwrap();
        assert_eq!(moved, 1);
        assert_eq!(fs::read_to_string(mount.join("a.txt")).unwrap(), "aaaa");
        assert_eq!(fs::read_to_string(mount.join("a (this device).txt")).unwrap(), "bbbb");
        assert!(!under.join("a.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stray_rescue_skips_excluded_roots() {
        let root = std::env::temp_dir().join(format!("rclover-stray-excl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (under, mount) = (root.join("under"), root.join("mount"));
        fs::create_dir_all(under.join("Work/build")).unwrap();
        fs::write(under.join("Work/build/out.bin"), "ignored").unwrap();
        fs::write(under.join("Work/note.txt"), "rescue me").unwrap();
        fs::create_dir_all(&mount).unwrap();
        let moved = walk_strays(&under, &mount, "", &[], &["Work/build".into()], &|_, _| {}).unwrap();
        assert_eq!(moved, 1);
        assert_eq!(fs::read_to_string(mount.join("Work/note.txt")).unwrap(), "rescue me");
        assert!(!mount.join("Work/build").exists());
        assert!(under.join("Work/build/out.bin").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unpinned_copy_moves_only_what_the_cloud_lacks() {
        crate::rclone::bisync::tests::with_rclone("prune", |c, d| {
            let (under, mount) = (d.join("under"), d.join("mount"));
            for (f, text) in [("Old/same.txt", "same"), ("Old/edited.txt", "mine"), ("Old/new.txt", "new")] {
                fs::create_dir_all(under.join("Old")).unwrap();
                fs::write(under.join(f), text).unwrap();
            }
            fs::create_dir_all(mount.join("Old")).unwrap();
            fs::write(mount.join("Old/same.txt"), "same").unwrap();
            fs::write(mount.join("Old/edited.txt"), "theirs").unwrap();
            let remote = mount.to_string_lossy().into_owned() + "/";
            let prune = |local: &Path, rel: &str| prune_identical(c, local, &format!("{remote}{rel}"));
            walk_strays(&under, &mount, "", &[], &[], &prune).unwrap();
            assert!(!mount.join("Old/same (this device).txt").exists(), "no duplicate of an unchanged file");
            assert_eq!(fs::read_to_string(mount.join("Old/edited (this device).txt")).unwrap(), "mine");
            assert_eq!(fs::read_to_string(mount.join("Old/new.txt")).unwrap(), "new");
            assert!(!under.join("Old").exists());
        });
    }
}
