//! One-time move from rclover's old name, Nuvolo: the config, cache and data folders, the
//! names rclone's two-way sync gave its state (they contain the cache folder's path), the
//! paths saved in home-folder moves, and the Start at Login entry.

use gtk::glib;
use std::fs;
use std::path::{Path, PathBuf};

const OLD: &str = "nuvolo";
const OLD_APP_ID: &str = "io.github.cyythanapong_netizen.Nuvolo";

/// Moves what Nuvolo kept under ~/.config, ~/.cache and ~/.local/share, unless rclover's own
/// folder exists already. Fails only while Nuvolo itself still runs: its rclone would keep
/// using the folders being moved.
pub fn from_nuvolo() -> Result<(), String> {
    let bases = [glib::user_config_dir(), glib::user_cache_dir(), glib::user_data_dir()];
    let todo: Vec<&PathBuf> = bases.iter().filter(|b| b.join(OLD).symlink_metadata().is_ok() && b.join("rclover").symlink_metadata().is_err()).collect();
    if todo.is_empty() {
        return Ok(());
    }
    if nuvolo_running() {
        return Err("Nuvolo, the old name of rclover, is still running. Quit it from its menu in the top bar (or run “pkill nuvolo”), then start rclover again.".to_owned());
    }
    for base in todo {
        let (old, new) = (base.join(OLD), base.join("rclover"));
        fs::rename(&old, &new).map_err(|e| format!("Could not move {} to {}: {e}", old.display(), new.display()))?;
        eprintln!("rclover: moved {} to {}", old.display(), new.display());
        if *base == glib::user_cache_dir() {
            rename_bisync_state(&new.join("bisync"), &old, &new);
        }
        if *base == glib::user_data_dir() {
            rewrite_jobs(&new.join("jobs"), &old, &new);
        }
    }
    let autostart = glib::user_config_dir().join("autostart");
    if fs::remove_file(autostart.join(format!("{OLD_APP_ID}.desktop"))).is_ok() {
        if let Err(e) = crate::ui::set_autostart(true) {
            eprintln!("rclover: could not keep Start at Login: {e}");
        }
    }
    Ok(())
}

fn nuvolo_running() -> bool {
    fs::read_dir("/proc").into_iter().flatten().flatten().any(|p| fs::read_to_string(p.path().join("comm")).is_ok_and(|c| c.trim() == OLD))
}

/// rclone names a pair's listings and locks after both paths, with `/` and `:` as `_` and
/// without the leading `/` (`home_me_.cache_nuvolo_offline_gdrive..gdrive_.path1.lst`).
/// rclover keeps its first-run markers in the `rclover` folder, named the same way but with
/// the leading `_`. Both are renamed, so the next sync carries on instead of starting afresh,
/// which would bring back files deleted in the cloud.
fn rename_bisync_state(workdir: &Path, old: &Path, new: &Path) {
    let key = |p: &Path| p.display().to_string().replace(['/', ':'], "_");
    let (from, to) = (key(old), key(new));
    let (from_rclone, to_rclone) = (from.trim_start_matches('_'), to.trim_start_matches('_'));
    let _ = fs::rename(workdir.join(OLD), workdir.join("rclover"));
    for dir in [workdir.to_path_buf(), workdir.join("rclover")] {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let renamed = match name.strip_prefix(&from) {
                Some(rest) => format!("{to}{rest}"),
                None => match name.strip_prefix(from_rclone) {
                    Some(rest) => format!("{to_rclone}{rest}"),
                    None => continue,
                },
            };
            let _ = fs::rename(entry.path(), dir.join(renamed));
        }
    }
}

/// Unfinished home-folder moves name their backup under the old data folder.
fn rewrite_jobs(jobs: &Path, old: &Path, new: &Path) {
    for entry in fs::read_dir(jobs).into_iter().flatten().flatten() {
        let p = entry.path();
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let (o, n) = (old.display().to_string(), new.display().to_string());
        if text.contains(&o) {
            let _ = fs::write(&p, text.replace(&o, &n));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_bisync_state_and_job_paths() {
        let root = std::env::temp_dir().join(format!("rclover-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (old, new) = (root.join("nuvolo"), root.join("rclover"));
        let key = |p: &Path| p.display().to_string().replace(['/', ':'], "_");
        let wd = new.join("bisync");
        fs::create_dir_all(wd.join("nuvolo")).unwrap();
        // rclone's listing has no leading `_`; rclover's marker has one.
        let listing = format!("{}_offline_gdrive..gdrive_.path1.lst", key(&old).trim_start_matches('_'));
        let marker = format!("{}_offline_gdrive..gdrive_..86f9db044c45c72c", key(&old));
        assert!(!listing.starts_with('_') && marker.starts_with('_'));
        fs::write(wd.join(&listing), "").unwrap();
        fs::write(wd.join("nuvolo").join(&marker), "").unwrap();
        fs::write(wd.join("other..x.lst"), "").unwrap();
        rename_bisync_state(&wd, &old, &new);
        assert!(wd.join(format!("{}_offline_gdrive..gdrive_.path1.lst", key(&new).trim_start_matches('_'))).exists(), "rclone's listing");
        assert!(wd.join("rclover").join(format!("{}_offline_gdrive..gdrive_..86f9db044c45c72c", key(&new))).exists(), "first-run marker");
        assert!(wd.join("other..x.lst").exists(), "unrelated names untouched");

        let jobs = new.join("jobs");
        fs::create_dir_all(&jobs).unwrap();
        fs::write(jobs.join("Music.json"), format!("{{\"backup\": \"{}/backups/Music-1\"}}", old.display())).unwrap();
        rewrite_jobs(&jobs, &old, &new);
        assert!(fs::read_to_string(jobs.join("Music.json")).unwrap().contains(&format!("{}/backups/Music-1", new.display())));
        let _ = fs::remove_dir_all(root);
    }
}
