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

/// The XDG folders: glib's id, the `XDG_<KEY>_DIR` key in user-dirs.dirs, the default name.
pub const FOLDERS: [(UserDirectory, &str, &str); 8] = [
    (UserDirectory::Desktop, "DESKTOP", "Desktop"),
    (UserDirectory::Documents, "DOCUMENTS", "Documents"),
    (UserDirectory::Downloads, "DOWNLOAD", "Downloads"),
    (UserDirectory::Music, "MUSIC", "Music"),
    (UserDirectory::Pictures, "PICTURES", "Pictures"),
    (UserDirectory::Videos, "VIDEOS", "Videos"),
    (UserDirectory::Templates, "TEMPLATES", "Templates"),
    (UserDirectory::PublicShare, "PUBLICSHARE", "Public"),
];

/// All XDG home folders, as configured in user-dirs.dirs. One set to ~ itself (which
/// xdg-user-dirs-update does at login when a link dangles because the drive is not mounted
/// yet) falls back to its default name, so it is still listed.
pub fn home_paths() -> impl Iterator<Item = (&'static str, PathBuf)> {
    let home = glib::home_dir();
    FOLDERS.iter().map(move |&(dir, key, name)| {
        (key, glib::user_special_dir(dir).filter(|p| p != &home).unwrap_or_else(|| home.join(name)))
    })
}

/// Where the folder lives inside a mount by default.
pub fn cloud_path(mount: &Path, home: &Path) -> PathBuf {
    mount.join(home.file_name().unwrap_or_default())
}

/// The cloud folder `home` uses: the link's target if it is linked into `mount`, else where the
/// job says, else the default.
pub fn cloud_of(home: &Path, mount: &Path, job: Option<&Job>) -> PathBuf {
    fs::read_link(home)
        .ok()
        .filter(|t| points_into(t, mount))
        .or_else(|| job.and_then(|j| j.cloud.clone()))
        .unwrap_or_else(|| cloud_path(mount, home))
}

/// `cloud` relative to `mount` ("Documents", "Photos/Phone"), as folder modes are keyed.
pub fn rel_in(mount: &Path, cloud: &Path) -> String {
    cloud
        .strip_prefix(mount)
        .map(Path::to_path_buf)
        .or_else(|_| crate::rclone::canonical_mount_path(cloud).strip_prefix(crate::rclone::canonical_mount_path(mount)).map(Path::to_path_buf))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| cloud.file_name().unwrap_or_default().to_string_lossy().into_owned())
}

/// True if `path` is inside `mount` (also `mount` itself).
pub fn is_inside(path: &Path, mount: &Path) -> bool {
    points_into(path, mount)
}

/// Keeps `XDG_<key>_DIR` pointing at `home` in user-dirs.dirs, and turns off
/// xdg-user-dirs-update (user-dirs.conf `enabled=False`): at login it runs before the drive is
/// mounted, sees the linked folder dangling and resets it to ~, which drops its icon in Files.
/// Also clears what Flatpak apps kept of the folder's old form (see [`clear_sandbox_leftovers`]).
pub fn keep_user_dir(key: &str, home: &Path) -> io::Result<()> {
    clear_sandbox_leftovers(&flatpak_apps(), &home.file_name().unwrap_or_default().to_string_lossy());
    let cfg = glib::user_config_dir();
    fs::create_dir_all(&cfg)?;
    let conf = cfg.join("user-dirs.conf");
    let old = fs::read_to_string(&conf).unwrap_or_default();
    if !old.lines().any(|l| l.trim().eq_ignore_ascii_case("enabled=false")) {
        let kept: String = old.lines().filter(|l| !l.trim_start().starts_with("enabled")).map(|l| l.to_owned() + "\n").collect();
        fs::write(&conf, format!("{kept}enabled=False\n"))?;
    }
    let dirs = cfg.join("user-dirs.dirs");
    let old = fs::read_to_string(&dirs).unwrap_or_default();
    let new = set_user_dir(&old, key, home, &glib::home_dir());
    if new != old {
        let tmp = dirs.with_extension("dirs.tmp");
        fs::write(&tmp, new)?;
        fs::rename(tmp, &dirs)?;
        glib::reload_user_special_dirs_cache();
    }
    Ok(())
}

/// Where Flatpak apps keep their own home: ~/.var/app/<app id>.
pub fn flatpak_apps() -> PathBuf {
    glib::home_dir().join(".var").join("app")
}

/// A Flatpak app that may see `~/Music` keeps, in its own home, a link to where `~/Music`
/// leads, or an empty folder (where `~/Music` itself was mounted, while it was a real folder).
/// Once `~/Music` becomes a link, or a link to another place, or a real folder again, the
/// app no longer starts ("bwrap: Can't make symlink at ~/Music: destination exists and is not
/// a symlink"). Flatpak makes both anew at each start, so they are removed: links, and empty
/// folders only. Returns what was removed.
pub fn clear_sandbox_leftovers(apps: &Path, name: &str) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    if name.is_empty() {
        return removed;
    }
    for app in fs::read_dir(apps).into_iter().flatten().flatten() {
        let p = app.path().join(name);
        let gone = match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => fs::remove_file(&p).is_ok(),
            Ok(m) if m.is_dir() => fs::remove_dir(&p).is_ok(), // fails, as it should, unless empty
            _ => false,
        };
        if gone {
            eprintln!("rclover: removed {}, left by Flatpak; the app makes it anew", p.display());
            removed.push(p);
        }
    }
    removed
}

/// Runs [`keep_user_dir`] for every home folder linked into one of `mounts`, repairing entries
/// that xdg-user-dirs-update reset to ~ before rclover turned it off.
pub fn keep_linked_user_dirs(mounts: &[PathBuf]) {
    for (key, home) in home_paths() {
        if fs::read_link(&home).is_ok_and(|t| mounts.iter().any(|m| points_into(&t, m))) {
            if let Err(e) = keep_user_dir(key, &home) {
                eprintln!("rclover: could not keep {} in user-dirs.dirs: {e}", home.display());
            }
        }
    }
}

/// user-dirs.dirs text with `XDG_<key>_DIR` set to `path` (written as "$HOME/..." when in ~).
fn set_user_dir(text: &str, key: &str, path: &Path, home: &Path) -> String {
    let value = match path.strip_prefix(home) {
        Ok(rel) => format!("$HOME/{}", rel.display()),
        Err(_) => path.display().to_string(),
    };
    let var = format!("XDG_{key}_DIR=");
    let line = format!("{var}\"{value}\"");
    let mut found = false;
    let mut out: String = text
        .lines()
        .map(|l| if l.starts_with(&var) { found = true; line.clone() } else { l.to_owned() })
        .map(|l| l + "\n")
        .collect();
    if !found {
        out += &(line + "\n");
    }
    out
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
    /// The cloud folder, when not the default `mount/<name>` (older job files have none).
    #[serde(default)]
    pub cloud: Option<PathBuf>,
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

/// Replaces `home` with a symlink to `cloud` (a folder inside `mount`, created if missing) and
/// records a [`Op::Link`] job. The contents still have to be copied with [`finish`] (slow, run
/// it off the main thread). A missing `home` is created empty first.
pub fn link(home: &Path, cloud: &Path, mount: &Path, backups: &Path, jobs: &Path) -> io::Result<()> {
    if !is_inside(cloud, mount) {
        return Err(io::Error::other(format!("{} is not on the drive", cloud.display())));
    }
    if home.symlink_metadata().is_err() {
        fs::create_dir_all(home)?;
    }
    if fs::symlink_metadata(home)?.file_type().is_symlink() {
        return Err(io::Error::other(format!("{} is already a link", home.display())));
    }
    fs::create_dir_all(cloud)?;
    let backup = backup_path(backups, home);
    fs::create_dir_all(backups)?;
    // Recorded first, so a crash at any later point is seen as an unfinished move.
    let job = Job { op: Op::Link, backup: Some(backup.clone()), mount: Some(mount.to_path_buf()), cloud: Some(cloud.to_path_buf()), error: None };
    write_job(jobs, home, &job)?;
    // Same filesystem (both in ~), so this is an atomic rename, not a copy.
    if let Err(e) = fs::rename(home, &backup) {
        clear_job(jobs, home);
        return Err(e);
    }
    if let Err(e) = std::os::unix::fs::symlink(cloud, home) {
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
    let cloud = fs::read_link(home)?;
    write_job(jobs, home, &Job { op: Op::Unlink, backup: None, mount: Some(mount.to_path_buf()), cloud: Some(cloud), error: None })
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
    let cloud = cloud_of(home, mount, Some(job));
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
                std::os::unix::fs::symlink(cloud_of(home, mount, Some(&job)), home)?;
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
    for (_, home) in home_paths() {
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
            let _ = write_job(jobs, &home, &Job { op: Op::Link, backup: Some(backup), mount: None, cloud: None, error: None });
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

        link(&home, &cloud_path(&mount, &home), &mount, &root.join("backups"), &jobs).unwrap();
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
    fn links_to_a_cloud_folder_with_another_name() {
        let root = tmp("custom");
        let (home, mount, jobs) = (root.join("Pictures"), root.join("mount"), root.join("jobs"));
        let cloud = mount.join("Photos/Phone");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("a.jpg"), "x").unwrap();
        assert!(link(&home, &root.join("elsewhere"), &mount, &root.join("backups"), &jobs).is_err(), "only into the drive");
        link(&home, &cloud, &mount, &root.join("backups"), &jobs).unwrap();
        let Status::Unfinished(job) = status(&home, &mount, &jobs) else { panic!() };
        finish(&home, &mount, &job, &AtomicUsize::new(0)).unwrap();
        clear_job(&jobs, &home);
        assert_eq!(fs::read_link(&home).unwrap(), cloud);
        assert_eq!(rel_in(&mount, &cloud_of(&home, &mount, None)), "Photos/Phone");
        assert!(cloud.join("a.jpg").exists());

        start_unlink(&home, &mount, &jobs).unwrap();
        let Status::Unfinished(job) = status(&home, &mount, &jobs) else { panic!() };
        finish(&home, &mount, &job, &AtomicUsize::new(0)).unwrap();
        assert_eq!(fs::read_to_string(home.join("a.jpg")).unwrap(), "x", "moved back from the custom folder");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clears_only_links_and_empty_folders_left_by_flatpak() {
        let root = tmp("flatpak");
        let (steam, other, notes) = (root.join("com.valvesoftware.Steam"), root.join("org.example.App"), root.join("org.example.Notes"));
        fs::create_dir_all(steam.join("Music")).unwrap(); // empty mount point
        fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink("../../home/me/Onedrive/Music", other.join("Music")).unwrap(); // stale link
        fs::create_dir_all(notes.join("Music")).unwrap();
        fs::write(notes.join("Music/song.ogg"), "keep").unwrap(); // the app's own files
        let removed = clear_sandbox_leftovers(&root, "Music");
        assert_eq!(removed.len(), 2);
        assert!(!steam.join("Music").exists() && other.join("Music").symlink_metadata().is_err());
        assert_eq!(fs::read_to_string(notes.join("Music/song.ogg")).unwrap(), "keep", "never a folder with files");
        assert!(clear_sandbox_leftovers(&root.join("missing"), "Music").is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sets_user_dirs_entries() {
        let home = Path::new("/home/me");
        let text = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_MUSIC_DIR=\"$HOME/\"\n";
        assert_eq!(
            set_user_dir(text, "MUSIC", &home.join("Music"), home),
            "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_MUSIC_DIR=\"$HOME/Music\"\n"
        );
        assert_eq!(set_user_dir("", "VIDEOS", Path::new("/data/Videos"), home), "XDG_VIDEOS_DIR=\"/data/Videos\"\n");
    }

    #[test]
    fn a_failed_move_to_the_cloud_can_be_undone() {
        let root = tmp("undo");
        let (home, mount, jobs) = (root.join("Pictures"), root.join("mount"), root.join("jobs"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&mount).unwrap();
        fs::write(home.join("cat.jpg"), "meow").unwrap();
        link(&home, &cloud_path(&mount, &home), &mount, &root.join("backups"), &jobs).unwrap();
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
        write_job(&jobs, &home, &Job { op: Op::Link, backup: Some(backup), mount: Some(mount.clone()), cloud: None, error: None }).unwrap();

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
        write_job(&jobs, &home, &Job { op: Op::Unlink, backup: None, mount: Some(mount.clone()), cloud: None, error: None }).unwrap();
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
        write_job(&jobs, &home, &Job { op: Op::Unlink, backup: None, mount: Some(mount.clone()), cloud: None, error: None }).unwrap();

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
