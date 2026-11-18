//! Keeping XDG home folders (Documents, Pictures, ...) in the cloud.
//!
//! Linking moves the folder into the mount and leaves a symlink in its place:
//! `~/Documents -> ~/Cloud/gdrive/Documents`. It never deletes anything: the original folder
//! is renamed to a backup first, then its contents are copied into the cloud folder, keeping
//! both copies when a name already exists there.

use gtk::glib::{self, UserDirectory};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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

/// True if `home` is a symlink into `mount`.
pub fn is_linked(home: &Path, mount: &Path) -> bool {
    fs::read_link(home).is_ok_and(|t| t.starts_with(mount))
}

/// Where originals are kept: ~/.local/share/nuvolo/backups (same filesystem as ~).
pub fn backup_root() -> PathBuf {
    glib::user_data_dir().join("nuvolo").join("backups")
}

fn backup_path(root: &Path, home: &Path) -> PathBuf {
    let name = home.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let stamp = glib::DateTime::now_local().ok().and_then(|d| d.format("%Y%m%d-%H%M%S").ok()).map(|s| s.to_string()).unwrap_or_default();
    root.join(format!("{name}-{stamp}"))
}

/// Replaces `home` with a symlink to its cloud copy. Returns the backup of the original,
/// whose contents still have to be copied with [`merge_into`] (slow, run it off the main thread).
pub fn link(home: &Path, mount: &Path, backups: &Path) -> io::Result<PathBuf> {
    let cloud = cloud_path(mount, home);
    if fs::symlink_metadata(home)?.file_type().is_symlink() {
        return Err(io::Error::other(format!("{} is already a link", home.display())));
    }
    fs::create_dir_all(&cloud)?;
    let backup = backup_path(backups, home);
    fs::create_dir_all(backups)?;
    // Same filesystem (both in ~), so this is an atomic rename, not a copy.
    fs::rename(home, &backup)?;
    if let Err(e) = std::os::unix::fs::symlink(&cloud, home) {
        fs::rename(&backup, home)?; // put it back
        return Err(e);
    }
    Ok(backup)
}

/// Turns the symlink back into a real folder holding a copy of the cloud folder.
/// The cloud copy is kept. Slow: run it off the main thread.
pub fn unlink(home: &Path, mount: &Path) -> io::Result<()> {
    if !is_linked(home, mount) {
        return Err(io::Error::other(format!("{} is not stored in the cloud", home.display())));
    }
    let staging = home.with_file_name(format!(".{}.nuvolo-restore", home.file_name().unwrap_or_default().to_string_lossy()));
    fs::create_dir_all(&staging)?;
    merge_into(&cloud_path(mount, home), &staging)?;
    fs::remove_file(home)?; // the symlink only
    fs::rename(&staging, home)
}

/// Copies everything in `from` into `to`. A file whose name is taken in `to` is copied
/// under a new name ("report (this device).pdf") instead of overwriting.
pub fn merge_into(from: &Path, to: &Path) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let kind = entry.file_type()?;
        let mut dst = to.join(entry.file_name());
        if kind.is_dir() {
            if dst.exists() && !dst.is_dir() {
                dst = free_name(&dst);
            }
            fs::create_dir_all(&dst)?;
            merge_into(&src, &dst)?;
        } else if kind.is_symlink() {
            let target = fs::read_link(&src)?;
            if dst.symlink_metadata().is_ok() {
                dst = free_name(&dst);
            }
            std::os::unix::fs::symlink(target, dst)?;
        } else {
            if dst.symlink_metadata().is_ok() {
                dst = free_name(&dst);
            }
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// "a.txt" -> "a (this device).txt", then "a (this device 2).txt", ... until unused.
fn free_name(p: &Path) -> PathBuf {
    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..)
        .map(|n| {
            let tag = if n == 1 { "this device".to_owned() } else { format!("this device {n}") };
            p.with_file_name(format!("{stem} ({tag}){ext}"))
        })
        .find(|c| c.symlink_metadata().is_err())
        .expect("an unused name exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nuvolo-homedirs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn merge_keeps_both_copies_and_never_overwrites() {
        let (from, to) = (tmp("from"), tmp("to"));
        fs::create_dir_all(from.join("sub")).unwrap();
        fs::write(from.join("a.txt"), "local").unwrap();
        fs::write(from.join("sub/b.txt"), "local b").unwrap();
        fs::write(to.join("a.txt"), "cloud").unwrap();
        merge_into(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "cloud");
        assert_eq!(fs::read_to_string(to.join("a (this device).txt")).unwrap(), "local");
        assert_eq!(fs::read_to_string(to.join("sub/b.txt")).unwrap(), "local b");
        merge_into(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(to.join("a (this device 2).txt")).unwrap(), "local");
        let _ = (fs::remove_dir_all(from), fs::remove_dir_all(to));
    }

    #[test]
    fn link_then_unlink_round_trips_without_losing_files() {
        let root = tmp("link");
        let (home, mount) = (root.join("Documents"), root.join("mount"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&mount).unwrap();
        fs::write(home.join("note.txt"), "hello").unwrap();

        let backup = link(&home, &mount, &root.join("backups")).unwrap();
        assert!(is_linked(&home, &mount));
        assert_eq!(fs::read_to_string(backup.join("note.txt")).unwrap(), "hello");
        merge_into(&backup, &cloud_path(&mount, &home)).unwrap();
        assert_eq!(fs::read_to_string(home.join("note.txt")).unwrap(), "hello"); // through the link

        unlink(&home, &mount).unwrap();
        assert!(!fs::symlink_metadata(&home).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(home.join("note.txt")).unwrap(), "hello");
        assert!(mount.join("Documents/note.txt").exists(), "cloud copy is kept");
        let _ = fs::remove_dir_all(root);
    }
}
