//! Keeping XDG home folders (Documents, Pictures, ...) in the cloud.
//!
//! Linking moves the folder into the mount and leaves a symlink in its place:
//! `~/Documents -> ~/Cloud/gdrive/Documents`. It never deletes anything: the original folder
//! is renamed to a backup first, then its contents are copied into the cloud folder, keeping
//! both copies when a name already exists there.
//!
//! A move in progress is recorded in a job file (`~/.local/share/rclover/jobs/Documents.json`)
//! before anything changes, and removed only once it has finished. So after a failure, a quit or
//! a crash, [`status`] knows the folder is half-moved and the move can be resumed or undone.
//! Copying is safe to repeat: a file already copied is skipped, not duplicated.
//!
//! - [`copy`]: the never-overwrite, resumable copy.
//! - [`runner`]: running moves in the background and resuming interrupted ones.

mod copy;
mod runner;

pub use copy::{merge_entry_with, merge_into, CopyPolicy};
pub use runner::{resume_all, run};

use gtk::glib::{self, UserDirectory};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;

pub const FOLDERS: [UserDirectory; 7] = [
    UserDirectory::Desktop,
    UserDirectory::Documents,
    UserDirectory::Downloads,
    UserDirectory::Music,
    UserDirectory::Pictures,
    UserDirectory::Videos,
    UserDirectory::Templates,
];

/// The folder in ~ (as configured in user-dirs.dirs), skipping ones set to ~ itself.
pub fn home_path(dir: UserDirectory) -> Option<PathBuf> {
    glib::user_special_dir(dir).filter(|p| p != &glib::home_dir())
}

/// Where the folder lives inside a mount.
pub fn cloud_path(mount: &Path, home: &Path) -> PathBuf {
    mount.join(home.file_name().unwrap_or_default())
}

/// True if `target` points inside `mount` (comparing both raw and canonicalized mount paths).
fn points_into(target: &Path, mount: &Path) -> bool {
    target.starts_with(mount)
        || crate::rclone::canonical_mount_path(target).starts_with(crate::rclone::canonical_mount_path(mount))
}

/// True if `home` is a symlink into `mount`.
pub fn is_linked(home: &Path, mount: &Path) -> bool {
    fs::read_link(home).is_ok_and(|t| points_into(&t, mount))
}

/// Where originals are kept: ~/.local/share/rclover/backups (same filesystem as ~).
pub fn backup_root() -> PathBuf {
    glib::user_data_dir().join("rclover").join("backups")
}

fn backup_path(root: &Path, home: &Path) -> PathBuf {
    let name = home.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let stamp = glib::DateTime::now_local().ok().and_then(|d| d.format("%Y%m%d-%H%M%S").ok()).map(|s| s.to_string()).unwrap_or_default();
    root.join(format!("{name}-{stamp}"))
}

/// Where job files are kept: ~/.local/share/rclover/jobs.
pub fn jobs_root() -> PathBuf {
    glib::user_data_dir().join("rclover").join("jobs")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Op {
    /// Moving to the cloud: copying `backup` into the cloud folder.
    Link,
    /// Moving back: copying the cloud folder into a hidden staging folder next to it.
    Unlink,
}

/// A move that has started and not finished.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub op: Op,
    /// The original folder, for [`Op::Link`].
    pub backup: Option<PathBuf>,
    /// The mount point this move targets, so recovery works even if `home` is missing.
    #[serde(default)]
    pub mount: Option<PathBuf>,
    /// Why the last attempt stopped, if it failed.
    pub error: Option<String>,
}

fn job_file(jobs: &Path, home: &Path) -> PathBuf {
    jobs.join(format!("{}.json", home.file_name().unwrap_or_default().to_string_lossy()))
}

pub fn read_job(jobs: &Path, home: &Path) -> Option<Job> {
    serde_json::from_slice(&fs::read(job_file(jobs, home)).ok()?).ok()
}

pub fn write_job(jobs: &Path, home: &Path, job: &Job) -> io::Result<()> {
    fs::create_dir_all(jobs)?;
    let f = job_file(jobs, home);
    let tmp = f.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(job).map_err(io::Error::other)?)?;
    fs::rename(tmp, f)
}

pub fn clear_job(jobs: &Path, home: &Path) {
    let _ = fs::remove_file(job_file(jobs, home));
}

/// Where a home folder stands, as far as `mount` is concerned.
#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    /// A real folder on this device.
    Local,
    /// A link into `mount`, fully copied.
    Linked,
    /// A link into another place (another account, or set up by hand).
    Elsewhere(PathBuf),
    /// A move into or out of `mount` that started and did not finish.
    Unfinished(Job),
}

pub fn status(home: &Path, mount: &Path, jobs: &Path) -> Status {
    let job = read_job(jobs, home);
    match fs::read_link(home) {
        Ok(t) if points_into(&t, mount) => job.map_or(Status::Linked, Status::Unfinished),
        Ok(t) => Status::Elsewhere(t),
        Err(_) => {
            if let Some(j) = job {
                let missing_home = home.symlink_metadata().is_err();
                let recoverable = match j.op {
                    // Crashed between rename(home, backup) and creating the symlink.
                    Op::Link => missing_home && j.backup.as_deref().is_some_and(Path::is_dir),
                    // Crashed between removing the symlink and renaming staging to home.
                    Op::Unlink => missing_home && staging(home).is_dir(),
                };
                if recoverable {
                    return Status::Unfinished(j);
                }
                // Otherwise a real folder is in place: either the move never got as far as
                // linking or a move back completed, so the job file is stale.
                clear_job(jobs, home);
            }
            Status::Local
        }
    }
}

/// Replaces `home` with a symlink to its cloud copy and records a [`Op::Link`] job. The
/// contents still have to be copied with [`finish`] (slow, run it off the main thread).
pub fn link(home: &Path, mount: &Path, backups: &Path, jobs: &Path) -> io::Result<()> {
    let cloud = cloud_path(mount, home);
    if fs::symlink_metadata(home)?.file_type().is_symlink() {
        return Err(io::Error::other(format!("{} is already a link", home.display())));
    }
    fs::create_dir_all(&cloud)?;
    let backup = backup_path(backups, home);
    fs::create_dir_all(backups)?;
    // Recorded first, so a crash at any later point is seen as an unfinished move.
    write_job(jobs, home, &Job { op: Op::Link, backup: Some(backup.clone()), mount: Some(mount.to_path_buf()), error: None })?;
    // Same filesystem (both in ~), so this is an atomic rename, not a copy.
    if let Err(e) = fs::rename(home, &backup) {
        clear_job(jobs, home);
        return Err(e);
    }
    if let Err(e) = std::os::unix::fs::symlink(&cloud, home) {
        fs::rename(&backup, home)?; // put it back
        clear_job(jobs, home);
        return Err(e);
    }
    Ok(())
}

/// Starts moving `home` back to this device: records an [`Op::Unlink`] job. Copy with [`finish`].
pub fn start_unlink(home: &Path, mount: &Path, jobs: &Path) -> io::Result<()> {
    if !is_linked(home, mount) {
        return Err(io::Error::other(format!("{} is not stored in the cloud", home.display())));
    }
    write_job(jobs, home, &Job { op: Op::Unlink, backup: None, mount: Some(mount.to_path_buf()), error: None })
}

/// Where a move back is assembled. One left by a move that Nuvolo (rclover's old name) started
/// is taken over under the new name.
fn staging(home: &Path) -> PathBuf {
    let name = home.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let new = home.with_file_name(format!(".{name}.rclover-restore"));
    let old = home.with_file_name(format!(".{name}.nuvolo-restore"));
    if !new.exists() && old.is_dir() {
        let _ = fs::rename(&old, &new);
    }
    new
}

/// Does (or resumes) the copying of an unfinished move. The caller clears the job: a move back
/// is complete on success, a move to the cloud once its uploads are done. Slow: run it off the
/// main thread.
pub fn finish(home: &Path, mount: &Path, job: &Job, copied: &AtomicUsize) -> io::Result<()> {
    let cloud = cloud_path(mount, home);
    match job.op {
        Op::Link => {
            let backup = job.backup.as_deref().ok_or_else(|| io::Error::other("the job file names no backup"))?;
            fs::create_dir_all(&cloud)?;
            // Recover from a crash between rename(home, backup) and creating the symlink.
            if home.symlink_metadata().is_err() {
                std::os::unix::fs::symlink(&cloud, home)?;
            }
            merge_into(backup, &cloud, copied)
        }
        Op::Unlink => {
            let staging = staging(home);
            fs::create_dir_all(&staging)?;
            if cloud.is_dir() {
                merge_into(&cloud, &staging, copied)?;
            }
            match fs::remove_file(home) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            fs::rename(&staging, home)
        }
    }
}

/// Gives up an unfinished move and puts things back as they were before it, without losing
/// anything: an undone move to the cloud gets the original folder back (files saved into the
/// cloud folder in the meantime stay there); an undone move back deletes only the partial copy.
pub fn undo(home: &Path, mount: &Path, jobs: &Path) -> io::Result<()> {
    let job = read_job(jobs, home).ok_or_else(|| io::Error::other("nothing to undo"))?;
    match job.op {
        Op::Link => {
            let backup = job.backup.ok_or_else(|| io::Error::other("the job file names no backup"))?;
            if !backup.is_dir() {
                return Err(io::Error::other(format!("the original folder {} is missing", backup.display())));
            }
            if is_linked(home, mount) {
                fs::remove_file(home)?; // the symlink only
            }
            fs::rename(&backup, home)?;
        }
        Op::Unlink => {
            // Recover from a crash after removing the symlink before renaming staging.
            if home.symlink_metadata().is_err() {
                std::os::unix::fs::symlink(cloud_path(mount, home), home)?;
            }
            match fs::remove_dir_all(staging(home)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    clear_job(jobs, home);
    Ok(())
}

/// Versions before 0.1.4 kept no job files, so a move to the cloud that failed looked
/// finished. Once (while `jobs` does not exist yet), each linked folder with a backup gets a
/// [`Op::Link`] job for its newest backup; resuming is safe because copying skips what is there.
pub fn adopt_old_moves(jobs: &Path, backups: &Path) {
    if jobs.exists() {
        return;
    }
    let _ = fs::create_dir_all(jobs);
    for home in FOLDERS.iter().filter_map(|&d| home_path(d)) {
        if !fs::symlink_metadata(&home).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        let prefix = format!("{}-", home.file_name().unwrap_or_default().to_string_lossy());
        // Names end in a sortable timestamp, so the greatest is the newest.
        let newest = fs::read_dir(backups)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)))
            .max();
        if let Some(backup) = newest {
            eprintln!("rclover: checking that {} was fully copied from {}", home.display(), backup.display());
            let _ = write_job(jobs, &home, &Job { op: Op::Link, backup: Some(backup), mount: None, error: None });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rclover-homedirs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn link_then_unlink_round_trips_without_losing_files() {
        let root = tmp("link");
        let (home, mount, jobs) = (root.join("Documents"), root.join("mount"), root.join("jobs"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&mount).unwrap();
        fs::write(home.join("note.txt"), "hello").unwrap();

        link(&home, &mount, &root.join("backups"), &jobs).unwrap();
        let Status::Unfinished(job) = status(&home, &mount, &jobs) else { panic!("a started move is unfinished") };
        assert_eq!(job.op, Op::Link);
        assert_eq!(fs::read_to_string(job.backup.as_ref().unwrap().join("note.txt")).unwrap(), "hello");
        finish(&home, &mount, &job, &AtomicUsize::new(0)).unwrap();
        assert_eq!(fs::read_to_string(home.join("note.txt")).unwrap(), "hello"); // through the link
        clear_job(&jobs, &home);
        assert_eq!(status(&home, &mount, &jobs), Status::Linked);

        start_unlink(&home, &mount, &jobs).unwrap();
        let Status::Unfinished(job) = status(&home, &mount, &jobs) else { panic!() };
        finish(&home, &mount, &job, &AtomicUsize::new(0)).unwrap();
        assert!(!fs::symlink_metadata(&home).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(home.join("note.txt")).unwrap(), "hello");
        assert!(mount.join("Documents/note.txt").exists(), "cloud copy is kept");
        assert_eq!(status(&home, &mount, &jobs), Status::Local, "the finished job is cleared");
        assert!(read_job(&jobs, &home).is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_move_to_the_cloud_can_be_undone() {
        let root = tmp("undo");
        let (home, mount, jobs) = (root.join("Pictures"), root.join("mount"), root.join("jobs"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&mount).unwrap();
        fs::write(home.join("cat.jpg"), "meow").unwrap();
        link(&home, &mount, &root.join("backups"), &jobs).unwrap();
        // The copy never ran (the drive was down): the link points at an empty cloud folder.
        assert!(matches!(status(&home, &mount, &jobs), Status::Unfinished(_)));
        undo(&home, &mount, &jobs).unwrap();
        assert_eq!(status(&home, &mount, &jobs), Status::Local);
        assert_eq!(fs::read_to_string(home.join("cat.jpg")).unwrap(), "meow");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn link_crash_before_symlink_is_recoverable() {
        let root = tmp("crash-link");
        let (home, mount, jobs, backups) = (root.join("Documents"), root.join("mount"), root.join("jobs"), root.join("backups"));
        fs::create_dir_all(&mount).unwrap();
        let backup = backups.join("Documents-20260101-000000");
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join("doc.txt"), "important").unwrap();
        // Simulate crash after rename(home, backup) and before creating the symlink: `home` is missing.
        write_job(&jobs, &home, &Job { op: Op::Link, backup: Some(backup), mount: Some(mount.clone()), error: None }).unwrap();

        let Status::Unfinished(job) = status(&home, &mount, &jobs) else {
            panic!("crash between rename and symlink must stay Unfinished");
        };
        assert!(read_job(&jobs, &home).is_some(), "job file must not be cleared");
        // Resuming recreates the symlink and copies the files.
        finish(&home, &mount, &job, &AtomicUsize::new(0)).unwrap();
        assert!(is_linked(&home, &mount));
        assert_eq!(fs::read_to_string(home.join("doc.txt")).unwrap(), "important");

        // Also test undo from the same crash point.
        fs::remove_file(&home).unwrap();
        undo(&home, &mount, &jobs).unwrap();
        assert_eq!(status(&home, &mount, &jobs), Status::Local);
        assert_eq!(fs::read_to_string(home.join("doc.txt")).unwrap(), "important");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn takes_over_a_move_back_started_under_the_old_name() {
        let root = tmp("legacy-staging");
        let (home, mount, jobs) = (root.join("Videos"), root.join("mount"), root.join("jobs"));
        fs::create_dir_all(cloud_path(&mount, &home)).unwrap();
        let old = root.join(".Videos.nuvolo-restore");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("clip.mp4"), "v").unwrap();
        write_job(&jobs, &home, &Job { op: Op::Unlink, backup: None, mount: Some(mount.clone()), error: None }).unwrap();
        let Status::Unfinished(job) = status(&home, &mount, &jobs) else { panic!("seen as unfinished") };
        finish(&home, &mount, &job, &AtomicUsize::new(0)).unwrap();
        assert_eq!(fs::read_to_string(home.join("clip.mp4")).unwrap(), "v");
        assert!(!old.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unlink_crash_before_rename_is_recoverable() {
        let root = tmp("crash-unlink");
        let (home, mount, jobs) = (root.join("Music"), root.join("mount"), root.join("jobs"));
        let cloud = cloud_path(&mount, &home);
        fs::create_dir_all(&cloud).unwrap();
        fs::write(cloud.join("song.flac"), "audio").unwrap();
        let stage = staging(&home);
        fs::create_dir_all(&stage).unwrap();
        fs::write(stage.join("song.flac"), "audio").unwrap();
        // Simulate crash after removing the symlink and before renaming staging to home.
        write_job(&jobs, &home, &Job { op: Op::Unlink, backup: None, mount: Some(mount.clone()), error: None }).unwrap();

        let Status::Unfinished(job) = status(&home, &mount, &jobs) else {
            panic!("crash between remove_file and rename(staging, home) must stay Unfinished");
        };
        assert!(read_job(&jobs, &home).is_some(), "job file must not be cleared");

        // Undo restores the symlink and cleans up staging.
        undo(&home, &mount, &jobs).unwrap();
        assert!(is_linked(&home, &mount));
        assert!(!stage.exists());

        // Now test finish from the same crash point.
        fs::remove_file(&home).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(stage.join("song.flac"), "audio").unwrap();
        write_job(&jobs, &home, &job).unwrap();
        finish(&home, &mount, &job, &AtomicUsize::new(0)).unwrap();
        assert!(!fs::symlink_metadata(&home).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(home.join("song.flac")).unwrap(), "audio");
        assert_eq!(status(&home, &mount, &jobs), Status::Local);
        let _ = fs::remove_dir_all(root);
    }
}
