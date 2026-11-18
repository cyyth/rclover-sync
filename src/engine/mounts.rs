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
use crate::rclone;
use gtk::gio;
use gtk::prelude::*;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountState {
    Mounting,
    Mounted,
    /// Not mounted, and why.
    Failed(String),
}

/// What the system knows about the network. False means no network at all (cable out,
/// Wi-Fi off); true does not promise the internet is reachable.
pub fn online() -> bool {
    gio::NetworkMonitor::default().is_network_available()
}

impl Engine {
    pub fn mount_state(&self, i: usize) -> MountState {
        let Some(a) = self.cfg.accounts.get(i) else { return MountState::Failed("no such account".into()) };
        if self.mounting.contains(&i) {
            MountState::Mounting
        } else if let Some(e) = self.mount_errors.get(&a.remote) {
            MountState::Failed(e.clone())
        } else {
            MountState::Mounted
        }
    }

    /// Accounts that are not mounted, for the tray.
    pub fn failed_mounts(&self) -> usize {
        self.mount_errors.len()
    }

    /// Starts mounting account `i` in the background, unless it is mounting already.
    pub fn mount(&mut self, i: usize) {
        if i >= self.cfg.accounts.len() || self.mounting.contains(&i) {
            return;
        }
        if !online() {
            return self.mounted(i, Err(NO_INTERNET.to_owned()));
        }
        if let Err(e) = self.prepare_mount_point(i) {
            return self.mounted(i, Err(e));
        }
        self.mounting.insert(i);
        let (client, cfg, a, tx) = (self.client.clone(), self.cfg.clone(), self.cfg.accounts[i].clone(), self.done_tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Done::Mounted(i, rclone::mount(&client, &cfg, &a)));
        });
    }

    /// Mounts every account that is not mounted, now: for the Retry button and for when the
    /// network comes back.
    pub fn retry_mounts(&mut self) {
        let failed: Vec<usize> = (0..self.cfg.accounts.len()).filter(|&i| matches!(self.mount_state(i), MountState::Failed(_))).collect();
        for i in failed {
            self.mount(i);
        }
    }

    /// Readies the mount folder: clears a dead mount left by a crashed rclone (it blocks both
    /// mkdir and mounting), creates the folder, and takes the handle that keeps the offline
    /// copies under it reachable once the mount covers it.
    fn prepare_mount_point(&mut self, i: usize) -> Result<(), String> {
        let mount = self.cfg.accounts[i].mount.clone();
        if rclone::mounted_paths().contains(&mount) && rclone::is_dead_mount(&mount) {
            eprintln!("nuvolo: clearing dead mount at {}", mount.display());
            rclone::lazy_unmount(&mount);
        }
        std::fs::create_dir_all(&mount).map_err(|e| format!("{}: {e}", mount.display()))?;
        self.hold_offline_dir(i);
        Ok(())
    }

    /// A mount finished, one way or the other.
    pub(super) fn mounted(&mut self, i: usize, res: Result<(), String>) {
        self.mounting.remove(&i);
        let Some(remote) = self.cfg.accounts.get(i).map(|a| a.remote.clone()) else { return };
        let id = format!("mount-{remote}");
        match res {
            Ok(()) => {
                if self.mount_errors.remove(&remote).is_some() {
                    notify::withdraw(&id);
                }
                self.sync_offline(i);
                self.refresh_pins(); // keep the live mount's cache warm too
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

    /// Remounts to apply filter or cache settings. While uploads are pending or the mount is
    /// busy (open in Files), it retries from `apply` instead. The new mount finishes in the
    /// background.
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
        if rclone::mounted_paths().contains(&a.mount) && self.client.call("mount/unmount", json!({ "mountPoint": a.mount })).is_err() {
            self.remount_later.insert(i);
            return Err("the drive is in use; it will apply once no window has it open".into());
        }
        self.remount_later.remove(&i);
        self.mount(i);
        Ok(())
    }

    /// Remounts any account whose mount has disappeared (crash of a FUSE helper, a manual
    /// unmount, a suspend glitch), so ~/Cloud never silently goes empty.
    pub(super) fn heal_mounts(&mut self, unhealthy: &HashSet<usize>) {
        // A mount that rclone finished after Nuvolo stopped waiting for it is up after all.
        let late: Vec<usize> = (0..self.cfg.accounts.len())
            .filter(|i| !unhealthy.contains(i) && matches!(self.mount_state(*i), MountState::Failed(_)))
            .collect();
        for i in late {
            self.mounted(i, Ok(()));
        }
        for &i in unhealthy {
            let Some(a) = self.cfg.accounts.get(i) else { continue };
            if self.remount_later.contains(&i) || self.mounting.contains(&i) {
                continue;
            }
            // A failing mount is retried every 10 s, a lost one right away.
            if self.mount_errors.contains_key(&a.remote) && self.ticks % 10 != 0 {
                continue;
            }
            if !self.mount_errors.contains_key(&a.remote) {
                eprintln!("nuvolo: {} is not mounted, mounting again", a.mount.display());
            }
            // rclone may still list the lost mount as its own; then it refuses to mount there.
            let _ = self.client.call("mount/unmount", json!({ "mountPoint": a.mount }));
            self.mount(i);
        }
    }

    pub fn is_mounted(&self, i: usize) -> bool {
        self.mount_state(i) == MountState::Mounted && self.cfg.accounts.get(i).is_some_and(|a| rclone::mounted_paths().contains(&a.mount))
    }

    /// The account whose mount holds `abs`, if that mount is up.
    pub fn mounted_account(&self, abs: &Path) -> Option<usize> {
        self.locate(abs).map(|(i, _)| i).filter(|&i| self.is_mounted(i))
    }
}
