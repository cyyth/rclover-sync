//! Offline copies: pinned (Always Offline) folders and files also exist as real files in the
//! mount folder itself, underneath the mount.
//!
//! While the drive is mounted, the mount covers them and `~/Cloud/<remote>` shows the live
//! drive. When Nuvolo is not running, or the drive could not be mounted (no internet), the
//! same paths show the offline copies. So links into pinned folders (home folders, dotfiles
//! in ~/.config) keep working at all times.
//!
//! How the covered folder stays reachable: before mounting, Nuvolo opens the mount folder and
//! keeps it open. Paths through that handle (`/proc/<pid>/fd/<n>/...`) lead under the mount,
//! not into it. A stable symlink, `~/.cache/nuvolo/offline/<remote>`, points there, so
//! rclone's two-way sync state survives restarts.
//!
//! After each mount and every 10 minutes, the copies are synced both ways with the cloud
//! ([`rclone::bisync`]). Anything else found under the mount (written while it was not
//! mounted, or a copy that is no longer pinned) is moved into the drive first, so it gets
//! uploaded instead of staying hidden under the mount.
//!
//! Pinned files are not loaded into rclone's VFS cache: the offline copies are the fixed
//! baseline of local storage, and the VFS cache limit is only the space for on-demand files.
// ponytail: while mounted, opening a pinned file reads it through the mount (from the network,
// into the VFS cache), not from its offline copy; if the network drops mid-session, only the
// offline copies under the mount have it. Upgrade path: unmount when the network goes away, so
// the offline copies show.

use super::{Done, Engine};
use crate::config::{Account, Config, ConflictPolicy, Mode};
use crate::errors::explain;
use crate::homedirs::merge_entry;
use crate::notify;
use crate::rclone::{self, Client, Scope};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;

impl Engine {
    /// Opens the mount folder before the mount covers it (a no-op once held, or if the mount
    /// is already up), and points `~/.cache/nuvolo/offline/<remote>` at it.
    pub(super) fn hold_offline_dir(&mut self, i: usize) {
        let a = &self.cfg.accounts[i];
        if self.offline_dirs.contains_key(&i) || rclone::mounted_paths().contains(&a.mount) {
            return;
        }
        let link = self.offline_link(&a.remote);
        let held = fs::File::open(&a.mount).and_then(|f| {
            fs::create_dir_all(link.parent().unwrap_or(Path::new("/")))?;
            match fs::remove_file(&link) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            std::os::unix::fs::symlink(format!("/proc/{}/fd/{}", std::process::id(), f.as_raw_fd()), &link)?;
            Ok(f)
        });
        match held {
            Ok(f) => {
                self.offline_dirs.insert(i, f);
            }
            Err(e) => eprintln!("nuvolo: no offline copies for {}: {e}", a.mount.display()),
        }
    }

    fn offline_link(&self, remote: &str) -> PathBuf {
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
        let a = self.cfg.accounts[i].clone();
        let job = OfflineJob {
            client: self.client.clone(),
            under: self.offline_link(&a.remote),
            workdir: self.cache_dir.join("bisync"),
            pinned: pinned_roots(&a),
            excludes: pinned_roots(&a).iter().map(|rel| excludes_below(&self.cfg, &a, rel)).collect(),
            policy: self.cfg.conflicts,
            force,
            account: a,
        };
        self.offline_running.insert(i);
        let tx = self.done_tx.clone();
        std::thread::spawn(move || {
            let res = job.run();
            let bytes = job.pinned.iter().map(|rel| du(&job.under.join(rel))).sum();
            let _ = tx.send(Done::Synced(i, res, bytes));
        });
    }

    /// An offline sync finished.
    pub(super) fn synced(&mut self, i: usize, res: Result<(), OfflineProblem>, bytes: u64) {
        self.offline_running.remove(&i);
        let Some(remote) = self.cfg.accounts.get(i).map(|a| a.remote.clone()) else { return };
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
    account: Account,
    /// The folder under the mount (through the held handle).
    under: PathBuf,
    workdir: PathBuf,
    pinned: Vec<String>,
    /// Exclude rules for each entry of `pinned`.
    excludes: Vec<Vec<String>>,
    policy: ConflictPolicy,
    force: bool,
}

impl OfflineJob {
    /// Moves loose files into the drive, then syncs each pinned folder, then the pinned single
    /// files (grouped by folder). Goes on past a failing folder, and reports the first failure.
    fn run(&self) -> Result<(), OfflineProblem> {
        let mount = &self.account.mount;
        let fail = |what: &str, raw: String| {
            eprintln!("nuvolo: offline copy of {what}: {raw}");
            OfflineProblem {
                text: format!("“{what}”: {}", explain(&raw)),
                can_force: raw.contains("too many deletes") || raw.contains("all files were changed"),
            }
        };
        rescue_strays(&self.under, mount, &self.pinned).map_err(|e| fail("files saved while the drive was not mounted", e.to_string()))?;
        let fs_ = self.account.fs();
        let mut first: Option<OfflineProblem> = None;
        let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (rel, excludes) in self.pinned.iter().zip(&self.excludes) {
            let local = if rel.is_empty() { self.under.clone() } else { self.under.join(rel) };
            // A pinned path is a file if either side says so (the cloud side, through the mount).
            if local.is_file() || mount.join(rel).is_file() {
                let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
                files.entry(dir.to_owned()).or_default().push(name.to_owned());
                continue;
            }
            let res = fs::create_dir_all(&local)
                .map_err(|e| e.to_string())
                .and_then(|_| rclone::bisync(&self.client, &local, &format!("{fs_}{rel}"), &self.workdir, Scope::All(excludes), self.policy, self.force))
                .and_then(|_| tidy_conflicts(&local).map_err(|e| e.to_string()));
            if let Err(e) = res {
                first.get_or_insert(fail(if rel.is_empty() { "whole drive" } else { rel }, e));
            }
        }
        for (dir, names) in files {
            let local = if dir.is_empty() { self.under.clone() } else { self.under.join(&dir) };
            // Single files keep bisync's own conflict name ("app.toml.conflict1"): a readable
            // name would fall outside the files this sync covers.
            let res = fs::create_dir_all(&local)
                .map_err(|e| e.to_string())
                .and_then(|_| rclone::bisync(&self.client, &local, &format!("{fs_}{dir}"), &self.workdir, Scope::Files(&names), self.policy, true));
            if let Err(e) = res {
                first.get_or_insert(fail(&names[0], e));
            }
        }
        first.map_or(Ok(()), Err)
    }
}

/// Gives conflict copies a readable name that keeps the extension, so they open as usual:
/// bisync's "notes.txt.conflict1" becomes "notes (conflict 1).txt". The next sync passes the
/// rename on to the cloud.
fn tidy_conflicts(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            tidy_conflicts(&path)?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((base, n)) = name.rsplit_once(".conflict").filter(|(b, n)| !b.is_empty() && !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())) else {
            continue;
        };
        let (stem, ext) = match base.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
            _ => (base, String::new()),
        };
        let target = (0..)
            .map(|k| dir.join(if k == 0 { format!("{stem} (conflict {n}){ext}") } else { format!("{stem} (conflict {n}-{k}){ext}") }))
            .find(|t| t.symlink_metadata().is_err())
            .expect("a free name exists");
        fs::rename(&path, target)?;
    }
    Ok(())
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

/// Moves into the mount whatever lies under it outside the pinned paths, so it is uploaded.
/// A name taken in the drive is kept side by side ("name (this device).ext"); a file already
/// there with the same size is taken as uploaded. Returns how many entries were moved.
fn rescue_strays(under: &Path, mount: &Path, pinned: &[String]) -> io::Result<usize> {
    // Never copy into the folder itself: that is what `mount` is when the mount is not up,
    // and removing the "copied" original would then lose it.
    let dev = |p: &Path| fs::metadata(p).map(|m| m.dev());
    if dev(under)? == dev(mount)? {
        return Err(io::Error::other("the drive is not mounted"));
    }
    walk_strays(under, mount, "", pinned)
}

fn walk_strays(under: &Path, mount: &Path, dir: &str, pinned: &[String]) -> io::Result<usize> {
    let mut moved = 0;
    for entry in fs::read_dir(under.join(dir))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if dir.is_empty() { name } else { format!("{dir}/{name}") };
        if pinned.iter().any(|p| inside(&rel, p)) {
            continue; // an offline copy
        }
        let is_dir = entry.file_type()?.is_dir();
        if is_dir && pinned.iter().any(|p| p.starts_with(&format!("{rel}/"))) {
            moved += walk_strays(under, mount, &rel, pinned)?; // holds an offline copy deeper down
            continue;
        }
        let to = mount.join(dir);
        fs::create_dir_all(&to)?;
        merge_entry(&entry, &to, &AtomicUsize::new(0))?;
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
        let d = std::env::temp_dir().join(format!("nuvolo-conflicts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sub")).unwrap();
        for f in ["notes.txt.conflict1", "sub/Makefile.conflict2", "photo.conflict.jpg", "plain.txt"] {
            fs::write(d.join(f), f).unwrap();
        }
        tidy_conflicts(&d).unwrap();
        assert_eq!(fs::read_to_string(d.join("notes (conflict 1).txt")).unwrap(), "notes.txt.conflict1");
        assert!(d.join("sub/Makefile (conflict 2)").exists());
        assert!(d.join("photo.conflict.jpg").exists(), "not a conflict copy");
        assert!(d.join("plain.txt").exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn strays_refuse_to_move_onto_the_same_disk() {
        // Without a mount, "under" and "mount" are the same folder: nothing may be moved.
        let d = std::env::temp_dir().join(format!("nuvolo-strays-{}", std::process::id()));
        fs::create_dir_all(d.join("loose")).unwrap();
        fs::write(d.join("loose/a.txt"), "keep me").unwrap();
        assert!(rescue_strays(&d, &d, &[]).is_err());
        assert_eq!(fs::read_to_string(d.join("loose/a.txt")).unwrap(), "keep me");
        let _ = fs::remove_dir_all(d);
    }
}
