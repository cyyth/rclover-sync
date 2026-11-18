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
// ponytail: pinned files are stored twice: the offline copy, and rclone's VFS cache that the
// live mount reads from. Upgrade path: reflink between the two on btrfs, or serve the mount's
// cache from the offline copy.

use super::{Done, Engine};
use crate::config::{Account, Config, Mode};
use crate::errors::explain;
use crate::homedirs::merge_entry;
use crate::notify;
use crate::rclone::{self, Client};
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
    pub fn offline_error(&self, remote: &str) -> Option<&String> {
        self.offline_errors.get(remote)
    }

    /// True while the offline copies of account `i` are syncing.
    pub fn offline_syncing(&self, i: usize) -> bool {
        self.offline_running.contains(&i)
    }

    /// Syncs the offline copies of account `i` with the cloud, in the background. Does
    /// nothing unless the drive is mounted and the folder under it is reachable.
    pub fn sync_offline(&mut self, i: usize) {
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
            account: a,
        };
        self.offline_running.insert(i);
        let tx = self.done_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done::Synced(i, job.run()));
        });
    }

    /// An offline sync finished.
    pub(super) fn synced(&mut self, i: usize, res: Result<(), String>) {
        self.offline_running.remove(&i);
        let Some(remote) = self.cfg.accounts.get(i).map(|a| a.remote.clone()) else { return };
        let id = format!("offline-{remote}");
        match res {
            Ok(()) => {
                if self.offline_errors.remove(&remote).is_some() {
                    notify::withdraw(&id);
                }
            }
            Err(e) => {
                eprintln!("nuvolo: offline copies of {remote}: {e}");
                if self.offline_errors.get(&remote) != Some(&e) {
                    notify::notify(&id, &format!("Offline copies of “{remote}” are not up to date"), &explain(&e));
                }
                self.offline_errors.insert(remote, e);
            }
        }
    }
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
}

impl OfflineJob {
    fn run(&self) -> Result<(), String> {
        let mount = &self.account.mount;
        rescue_strays(&self.under, mount, &self.pinned).map_err(|e| format!("moving files into the drive: {e}"))?;
        let fs_ = self.account.fs();
        for (rel, excludes) in self.pinned.iter().zip(&self.excludes) {
            let local = if rel.is_empty() { self.under.clone() } else { self.under.join(rel) };
            // A pinned path is a file if either side says so (the cloud side, through the mount).
            if local.is_file() || mount.join(rel).is_file() {
                fs::create_dir_all(local.parent().unwrap_or(&self.under)).map_err(|e| e.to_string())?;
                rclone::sync_file(&self.client, &local, &fs_, rel)?;
            } else {
                fs::create_dir_all(&local).map_err(|e| e.to_string())?;
                rclone::bisync(&self.client, &local, &format!("{fs_}{rel}"), &self.workdir, excludes)?;
            }
        }
        Ok(())
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
