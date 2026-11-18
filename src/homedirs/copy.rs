//! Copying that never overwrites and is safe to repeat: the building block of home-folder
//! moves and of moving loose files into the drive (`engine::offline`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Copies everything in `from` into `to`. A file whose name is taken in `to` is copied
/// under a new name ("report (this device).pdf") instead of overwriting, unless it has the
/// same size, which is taken as already copied: that makes a repeated or resumed merge skip
/// what it did before. `copied` counts files copied or skipped, for progress display.
// ponytail: "same name and size = same file" avoids reading cloud files back over the network;
// a different file of exactly the same size is not copied (it stays in the backup).
pub fn merge_into(from: &Path, to: &Path, copied: &AtomicUsize) -> io::Result<()> {
    for entry in fs::read_dir(from).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", from.display())))? {
        merge_entry(&entry?, to, copied)?;
    }
    Ok(())
}

/// Copies one entry of a folder (a file, link or whole subfolder) into `to`, by the rules of
/// [`merge_into`].
pub fn merge_entry(entry: &fs::DirEntry, to: &Path, copied: &AtomicUsize) -> io::Result<()> {
    {
        let src = entry.path();
        let kind = entry.file_type()?;
        let mut dst = to.join(entry.file_name());
        if kind.is_dir() {
            if dst.exists() && !dst.is_dir() {
                dst = free_name(&dst);
            }
            fs::create_dir_all(&dst)?;
            merge_into(&src, &dst, copied)?;
        } else if kind.is_symlink() {
            let target = fs::read_link(&src)?;
            if fs::read_link(&dst).is_ok_and(|t| t == target) {
                copied.fetch_add(1, Ordering::Relaxed);
                return Ok(()); // copied before
            }
            if dst.symlink_metadata().is_ok() {
                dst = free_name(&dst);
            }
            std::os::unix::fs::symlink(target, dst)?;
            copied.fetch_add(1, Ordering::Relaxed);
        } else {
            // The first of "a.txt", "a (this device).txt", ... that is free or already this file.
            let len = entry.metadata()?.len();
            let mut same = false;
            let wanted = dst.clone();
            dst = names(&wanted)
                .find(|c| match c.symlink_metadata() {
                    Err(_) => true,
                    Ok(m) => {
                        same = m.is_file() && m.len() == len;
                        same
                    }
                })
                .expect("an unused name exists");
            if same {
                copied.fetch_add(1, Ordering::Relaxed);
                return Ok(()); // copied before
            }
            // Copy under a temporary name, so an interrupted copy never looks finished.
            let part = dst.with_file_name(format!(".{}.nuvolo-part", dst.file_name().unwrap_or_default().to_string_lossy()));
            fs::copy(&src, &part).and_then(|_| fs::rename(&part, &dst)).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", src.display())))?;
            copied.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

/// "a.txt", then "a (this device).txt", "a (this device 2).txt", ...
fn names(p: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    let stem = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    std::iter::once(p.to_path_buf()).chain((1..).map(move |n| {
        let tag = if n == 1 { "this device".to_owned() } else { format!("this device {n}") };
        p.with_file_name(format!("{stem} ({tag}){ext}"))
    }))
}

/// The first of [`names`] that is not taken.
pub(super) fn free_name(p: &Path) -> PathBuf {
    names(p).find(|c| c.symlink_metadata().is_err()).expect("an unused name exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nuvolo-copy-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn merge_keeps_both_copies_and_can_be_repeated() {
        let (from, to) = (tmp("from"), tmp("to"));
        fs::create_dir_all(from.join("sub")).unwrap();
        fs::write(from.join("a.txt"), "local version").unwrap();
        fs::write(from.join("sub/b.txt"), "local b").unwrap();
        fs::write(to.join("a.txt"), "cloud").unwrap();
        let n = AtomicUsize::new(0);
        merge_into(&from, &to, &n).unwrap();
        assert_eq!(n.load(Ordering::Relaxed), 2, "two files copied");
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "cloud");
        assert_eq!(fs::read_to_string(to.join("a (this device).txt")).unwrap(), "local version");
        assert_eq!(fs::read_to_string(to.join("sub/b.txt")).unwrap(), "local b");
        // A resumed merge skips what is already there instead of making more copies.
        merge_into(&from, &to, &n).unwrap();
        assert!(!to.join("a (this device 2).txt").exists());
        assert!(!to.join("sub/b (this device).txt").exists());
        assert_eq!(fs::read_dir(&to).unwrap().count(), 3, "a.txt, a (this device).txt, sub; no .nuvolo-part left");
        let _ = (fs::remove_dir_all(from), fs::remove_dir_all(to));
    }

}
