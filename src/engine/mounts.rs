//! Mounting, remounting and healing mounts.
//!
//! Mounting reaches the cloud, so without internet it can take a minute to fail. It therefore
//! runs on a thread and reports back with [`Done::Mounted`]; meanwhile the account shows as
//! [`MountState::Mounting`]. When the system says there is no network at all, mounting fails
//! at once with "no internet connection", and [`Engine::retry_mounts`] runs again as soon
//! as the network returns (wired up in `main.rs`).

use super::{Done, Engine};
use crate::errors::{explain, NO_INTERNET};
use crate::notify;
use crate::rclone::{self, MountHealth};
use gtk::gio;
use gtk::prelude::*;
use serde_json::json;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountState {
    Mounting,
    Mounted,
    /// Not mounted, and why.
    Failed(String),
}

/// Why an asynchronous unmount was started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnmountKind {
    /// Applying new filter or cache settings; remount as soon as unmount finishes.
    Remount,
    /// Network dropped past the grace period; expose offline copies until network returns.
    Offline,
    /// Repeated FUSE EIO/timeouts; expose offline copies and heal on the next cycle.
    Unhealthy,
}

/// GNOME Files' "Move to Trash" on a FUSE drive moves files into `.Trash-<uid>` inside the
/// drive: a slow server-side move plus an uploaded `.trashinfo` per item, and the files stay
/// in the cloud using quota (issue #14). A plain file at that name makes GIO find no trash, so
/// GNOME Files deletes instead (one call), and the cloud keeps deleted files in its own trash.
/// Runs through the live mount: call it off the main thread.
///
/// A trash folder made before is kept, renamed to "Trashed in GNOME Files", for the user to
/// review: nothing in it is deleted.
pub fn block_gnome_trash(mount: &Path) -> std::io::Result<()> {
    // SAFETY: getuid cannot fail and touches no memory.
    let trash = mount.join(format!(".Trash-{}", unsafe { libc::getuid() }));
    match std::fs::symlink_metadata(&trash) {
        Ok(m) if !m.is_dir() => return Ok(()), // already blocked
        Ok(_) => {
            let kept = (1..)
                .map(|n| mount.join(if n == 1 { "Trashed in GNOME Files".to_owned() } else { format!("Trashed in GNOME Files {n}") }))
                .find(|p| p.symlink_metadata().is_err())
                .expect("a free name exists");
            std::fs::rename(&trash, &kept)?;
            eprintln!("nuvolo: moved GNOME Files' trash in the drive to {}", kept.display());
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        Err(_) => {}
    }
    std::fs::write(&trash, b"")
}

/// What the system knows about the network. False means no network at all (cable out,
/// Wi-Fi off); true does not promise the internet is reachable.
pub fn online() -> bool {
    gio::NetworkMonitor::default().is_network_available()
}

impl Engine {
    pub fn mount_state(&self, i: usize) -> MountState {
        let Some(a) = self.cfg.accounts.get(i) else { return MountState::Failed("no such account".into()) };
        if self.rclone_starting || self.mounting.contains(&i) {
            MountState::Mounting
        } else if let Some(err) = &self.rclone_error {
            MountState::Failed(err.clone())
        } else if let Some(e) = self.mount_errors.get(&a.remote) {
            MountState::Failed(e.clone())
        } else {
            MountState::Mounted
        }
    }

    /// Accounts that are not mounted, for the tray.
    pub fn failed_mounts(&self) -> usize {
        if self.rclone_error.is_some() {
            return self.cfg.accounts.len().max(1);
        }
        self.mount_errors.len()
    }

    /// Starts mounting account `i` in the background, unless it is mounting already.
    pub fn mount(&mut self, i: usize) {
        self.mount_inner(i, false);
    }

    fn mount_inner(&mut self, i: usize, unmount_first: bool) {
        if i >= self.cfg.accounts.len() || self.mounting.contains(&i) || self.unmounting.contains(&i) {
            return;
        }
        if self.rclone_starting {
            return;
        }
        let Some(client) = self.client.clone() else {
            if self.rclone_error.is_some() {
                self.retry_rclone();
            }
            return;
        };
        if !online() {
            return self.mounted(i, Err(NO_INTERNET.to_owned()), None);
        }
        self.mounting.insert(i);
        let gen = self.rclone_gen;
        let cfg = self.cfg.clone();
        let a = self.cfg.accounts[i].clone();
        let caps = self.caps;
        let need_offline_handle = !self.offline_dirs.contains_key(&i);
        let offline_link = self.offline_link(&a.remote);
        let tx = self.done_tx.clone();
        std::thread::spawn(move || {
            let mount_point = rclone::canonical_mount_path(&a.mount);
            if unmount_first {
                let _ = client.call_for("mount/unmount", json!({ "mountPoint": mount_point }), Duration::from_secs(10));
            }
            let held = match prepare_mount_point_bg(&mount_point, need_offline_handle, &offline_link) {
                Ok(h) => h,
                Err(e) => {
                    let _ = tx.send(Done::Mounted(gen, i, Err(e), None));
                    return;
                }
            };
            let res = rclone::mount(&client, &cfg, &a, caps);
            let _ = tx.send(Done::Mounted(gen, i, res, held));
        });
    }

    /// Mounts every account that is not mounted, now: for the Retry button and for when the
    /// network comes back.
    pub fn retry_mounts(&mut self) {
        if self.rclone_error.is_some() {
            self.retry_rclone();
            return;
        }
        let failed: Vec<usize> = (0..self.cfg.accounts.len()).filter(|&i| matches!(self.mount_state(i), MountState::Failed(_))).collect();
        for i in failed {
            self.mount(i);
        }
    }

    /// A mount finished, one way or the other.
    pub(super) fn mounted(&mut self, i: usize, res: Result<(), String>, held: Option<File>) {
        self.mounting.remove(&i);
        if let Some(f) = held {
            self.offline_dirs.entry(i).or_insert(f);
        }
        let Some(remote) = self.cfg.accounts.get(i).map(|a| a.remote.clone()) else { return };
        let id = format!("mount-{remote}");
        match res {
            Ok(()) => {
                self.unhealthy_strikes.remove(&i);
                if self.mount_errors.remove(&remote).is_some() {
                    notify::withdraw(&id);
                }
                let mount = self.cfg.accounts[i].mount.clone();
                std::thread::spawn(move || {
                    if let Err(e) = block_gnome_trash(&mount) {
                        eprintln!("nuvolo: {}: could not turn off GNOME Files' trash: {e}", mount.display());
                    }
                });
                self.sync_offline(i);
            }
            Err(e) => {
                eprintln!("nuvolo: mounting {remote}: failed: {e}");
                // One notification per new problem, not one per retry.
                if self.mount_errors.get(&remote) != Some(&e) {
                    notify::notify(&id, &format!("“{remote}” is not available"), &explain(&e));
                }
                self.mount_errors.insert(remote, e);
            }
        }
    }

    /// Uploads waiting or running, as of the last tick.
    pub fn pending_uploads(&self, remote: &str) -> u64 {
        self.vfs.get(remote).map_or(0, super::uploads_in)
    }

    /// Remounts to apply filter or cache settings. Runs unmount off the main thread so a busy
    /// or slow rclone never freezes GTK. While uploads are pending or the mount is busy (open
    /// in Files), it retries from `apply` instead.
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
        if self.unmounting.contains(&i) || self.mounting.contains(&i) {
            return Ok(());
        }
        self.remount_later.remove(&i);
        self.start_unmount(i, UnmountKind::Remount);
        Ok(())
    }

    /// Unmounts every mounted drive when the network has been down past the grace period, so
    /// pinned offline copies underneath the mount points become immediately readable.
    pub fn unmount_for_offline(&mut self) {
        if online() {
            return;
        }
        let mounted: Vec<usize> = (0..self.cfg.accounts.len())
            .filter(|&i| self.mount_state(i) == MountState::Mounted && !self.unmounting.contains(&i))
            .collect();
        for i in mounted {
            eprintln!("nuvolo: network unavailable; unmounting {} to expose offline copies", self.cfg.accounts[i].mount.display());
            self.start_unmount(i, UnmountKind::Offline);
        }
    }

    fn start_unmount(&mut self, i: usize, kind: UnmountKind) {
        let Some(client) = self.client.clone() else { return };
        let Some(a) = self.cfg.accounts.get(i) else { return };
        let mount_point = rclone::canonical_mount_path(&a.mount);
        let gen = self.rclone_gen;
        let tx = self.done_tx.clone();
        self.unmounting.insert(i);
        std::thread::spawn(move || {
            let mounted = rclone::mounted_paths();
            let res = if rclone::is_mounted_path(&mounted, &mount_point) {
                client
                    .call_for("mount/unmount", json!({ "mountPoint": mount_point }), Duration::from_secs(10))
                    .map(|_| ())
            } else {
                Ok(())
            };
            let _ = tx.send(Done::Unmounted(gen, i, res, kind));
        });
    }

    pub(super) fn unmounted(&mut self, i: usize, res: Result<(), String>, kind: UnmountKind) {
        self.unmounting.remove(&i);
        let Some(remote) = self.cfg.accounts.get(i).map(|a| a.remote.clone()) else { return };
        match (kind, res) {
            (UnmountKind::Remount, Ok(())) => {
                self.remount_later.remove(&i);
                self.mount(i);
            }
            (UnmountKind::Remount, Err(_)) => {
                // Mount is busy (open in Files): retry on the next health tick.
                self.remount_later.insert(i);
            }
            (UnmountKind::Offline, Ok(())) => {
                self.mount_errors.insert(remote, NO_INTERNET.to_owned());
            }
            (UnmountKind::Offline, Err(e)) => {
                eprintln!("nuvolo: could not unmount {remote} for offline mode (busy?): {e}");
            }
            (UnmountKind::Unhealthy, Ok(())) => {
                self.unhealthy_strikes.remove(&i);
                if online() {
                    self.mount(i);
                } else {
                    self.mount_errors.insert(remote, NO_INTERNET.to_owned());
                }
            }
            (UnmountKind::Unhealthy, Err(e)) => {
                eprintln!("nuvolo: could not unmount unhealthy mount {remote}: {e}");
            }
        }
    }

    /// Remounts any account whose mount has disappeared or become unhealthy (crash of a FUSE
    /// helper, a manual unmount, EIO/timeouts, a suspend glitch), so ~/Cloud never silently
    /// goes empty or wedged.
    pub(super) fn heal_mounts(&mut self, unhealthy: &HashMap<usize, MountHealth>) {
        // Clear strike counts for healthy accounts and recover mounts that came up late.
        let late: Vec<usize> = (0..self.cfg.accounts.len())
            .filter(|i| !unhealthy.contains_key(i))
            .collect();
        for i in late {
            self.unhealthy_strikes.remove(&i);
            if matches!(self.mount_state(i), MountState::Failed(_)) {
                self.mounted(i, Ok(()), None);
            }
        }
        for (&i, &health) in unhealthy {
            let Some(a) = self.cfg.accounts.get(i) else { continue };
            if self.remount_later.contains(&i) || self.mounting.contains(&i) || self.unmounting.contains(&i) {
                continue;
            }
            match health {
                MountHealth::Unhealthy => {
                    let strikes = self.unhealthy_strikes.entry(i).or_insert(0);
                    *strikes += 1;
                    if *strikes >= 2 {
                        eprintln!("nuvolo: {} failed health checks ({} strikes), unmounting to recover", a.mount.display(), *strikes);
                        self.start_unmount(i, UnmountKind::Unhealthy);
                    }
                }
                MountHealth::Dead => {
                    // A failing mount is retried every 10 s, a lost one right away.
                    if self.mount_errors.contains_key(&a.remote) && self.ticks % 10 != 0 {
                        continue;
                    }
                    if !self.mount_errors.contains_key(&a.remote) {
                        eprintln!("nuvolo: {} is not mounted, mounting again", a.mount.display());
                    }
                    self.mount_inner(i, true);
                }
                MountHealth::Healthy => {}
            }
        }
    }

    pub fn is_mounted(&self, i: usize) -> bool {
        self.mount_state(i) == MountState::Mounted
            && self.cfg.accounts.get(i).is_some_and(|a| rclone::is_mounted_path(&rclone::mounted_paths(), &a.mount))
    }

    /// The account whose mount holds `abs`, if that mount is up.
    pub fn mounted_account(&self, abs: &Path) -> Option<usize> {
        self.locate(abs).map(|(i, _)| i).filter(|&i| self.is_mounted(i))
    }
}

/// Readies the mount folder off the main thread: clears a dead mount left by a crashed rclone,
/// creates the folder, and opens an fd to keep offline copies under it reachable once covered.
fn prepare_mount_point_bg(mount: &Path, need_offline_handle: bool, offline_link: &Path) -> Result<Option<File>, String> {
    let mounted = rclone::mounted_paths();
    if rclone::is_mounted_path(&mounted, mount) && rclone::probe_mount(mount, Duration::from_secs(5)) == MountHealth::Dead {
        eprintln!("nuvolo: clearing dead mount at {}", mount.display());
        rclone::lazy_unmount(mount);
    }
    fs::create_dir_all(mount).map_err(|e| format!("{}: {e}", mount.display()))?;
    if !need_offline_handle || rclone::is_mounted_path(&rclone::mounted_paths(), mount) {
        return Ok(None);
    }
    let held = File::open(mount).and_then(|f| {
        fs::create_dir_all(offline_link.parent().unwrap_or(Path::new("/")))?;
        match fs::remove_file(offline_link) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::os::unix::fs::symlink(format!("/proc/{}/fd/{}", std::process::id(), f.as_raw_fd()), offline_link)?;
        Ok(f)
    });
    match held {
        Ok(f) => Ok(Some(f)),
        Err(e) => {
            eprintln!("nuvolo: no offline copies for {}: {e}", mount.display());
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_trash_is_blocked_and_an_old_one_kept() {
        let d = std::env::temp_dir().join(format!("nuvolo-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let trash = d.join(format!(".Trash-{}", unsafe { libc::getuid() }));
        std::fs::create_dir_all(trash.join("files")).unwrap();
        std::fs::write(trash.join("files/old.pdf"), "kept").unwrap();
        block_gnome_trash(&d).unwrap();
        assert!(trash.is_file(), "a placeholder file now blocks the trash");
        assert_eq!(std::fs::read_to_string(d.join("Trashed in GNOME Files/files/old.pdf")).unwrap(), "kept");
        block_gnome_trash(&d).unwrap(); // again: nothing changes
        assert!(!d.join("Trashed in GNOME Files 2").exists());
        let _ = std::fs::remove_dir_all(d);
    }
}
