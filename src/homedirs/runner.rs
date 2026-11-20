//! Running home-folder moves in the background, and resuming interrupted ones. Works without
//! the window: progress is in `Engine::jobs`, outcomes are notifications.

use super::{clear_job, finish, home_path, jobs_root, read_job, status, write_job, Job, Op, Status, FOLDERS};
use crate::engine::{Progress, Shared};
use gtk::{gio, glib};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Consecutive `vfs/queue` failures before pausing the upload wait (~20 s).
const MAX_UPLOAD_POLL_FAILURES: u32 = 10;

/// Number of entries in a `vfs/queue` reply belonging to `folder_name`.
pub(super) fn queued_uploads_for(v: &Value, folder_name: &str) -> usize {
    let prefix = format!("{folder_name}/");
    v["queue"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|item| {
                    item["name"]
                        .as_str()
                        .is_some_and(|n| n == folder_name || n.starts_with(&prefix))
                })
                .count()
        })
        .unwrap_or(0)
}

/// Runs (or resumes) the unfinished move of `home` in the background, if it is not running
/// already. Works without the window: progress is in `Engine::jobs`, the outcome is a
/// notification. A move to the cloud counts as done only once rclone has uploaded the files.
pub fn run(e: &Shared, home: PathBuf, mount: PathBuf, job: Job) {
    if e.borrow().jobs.contains_key(&home) {
        return;
    }
    let count = Arc::new(AtomicUsize::new(0));
    e.borrow_mut().jobs.insert(home.clone(), Progress { op: job.op, uploading: false, count: count.clone() });
    crate::ui::refresh_home();
    let e = e.clone();
    glib::spawn_future_local(async move {
        let name = home.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let id = format!("home-{name}");
        let (h, m, j) = (home.clone(), mount.clone(), job.clone());
        let res = gio::spawn_blocking(move || finish(&h, &m, &j, &count).map_err(|e| e.to_string()))
            .await
            .unwrap_or_else(|_| Err("the copy crashed".to_owned()));
        if let Err(err) = res {
            // Kept as an unfinished move with its reason; the window offers Resume and Undo.
            let _ = write_job(&jobs_root(), &home, &Job { error: Some(err.clone()), ..job.clone() });
            e.borrow_mut().jobs.remove(&home);
            crate::ui::refresh_home();
            let what = if job.op == Op::Link { "to the cloud" } else { "back to this device" };
            return crate::notify::notify(&id, &format!("Could not move {name} {what}"), &format!("{} Nothing was lost. Resume or undo it in Nuvolo → Preferences → Home Folders.", crate::errors::explain(&err)));
        }
        if job.op == Op::Link {
            // The files are in the cache; wait until rclone has uploaded the moved folder.
            // Re-fetch client on each poll in case rclone restarted, and stop after repeated failures.
            let mut failures: u32 = 0;
            loop {
                let target = {
                    let eng = e.borrow();
                    eng.mounted_account(&mount)
                        .and_then(|i| eng.client.clone().map(|c| (c, eng.cfg.accounts[i].fs())))
                };
                let left = match target {
                    Some((client, fs_)) => {
                        let folder = name.clone();
                        gio::spawn_blocking(move || {
                            client
                                .call_local("vfs/queue", serde_json::json!({ "fs": fs_ }))
                                .map(|v| queued_uploads_for(&v, &folder))
                        })
                        .await
                        .ok()
                        .and_then(Result::ok)
                    }
                    None => None,
                };
                match left {
                    Some(0) => break,
                    Some(n) => {
                        failures = 0;
                        if let Some(p) = e.borrow_mut().jobs.get_mut(&home) {
                            p.uploading = true;
                            p.count.store(n, Ordering::Relaxed);
                        }
                        crate::ui::refresh_home();
                    }
                    None => {
                        failures += 1;
                        if failures >= MAX_UPLOAD_POLL_FAILURES {
                            let err = "lost contact with the drive while waiting for uploads".to_owned();
                            let _ = write_job(&jobs_root(), &home, &Job { error: Some(err.clone()), ..job.clone() });
                            e.borrow_mut().jobs.remove(&home);
                            crate::ui::refresh_home();
                            return crate::notify::notify(
                                &id,
                                &format!("Upload of {name} was interrupted"),
                                "Nothing was lost. Nuvolo can resume once the drive is mounted again (Preferences → Home Folders).",
                            );
                        }
                        if let Some(p) = e.borrow_mut().jobs.get_mut(&home) {
                            p.uploading = true;
                        }
                        crate::ui::refresh_home();
                    }
                }
                glib::timeout_future_seconds(2).await;
            }
        }
        clear_job(&jobs_root(), &home);
        e.borrow_mut().jobs.remove(&home);
        crate::ui::refresh_home();
        if job.op == Op::Link {
            let backup = job.backup.map(|b| b.display().to_string()).unwrap_or_default();
            crate::notify::notify(&id, &format!("{name} is in the cloud"), &format!("Everything is uploaded. Your original folder is kept in {backup}; delete it once you have checked the cloud copy."));
        } else {
            crate::notify::notify(&id, &format!("{name} is back on this device"), "The cloud copy is kept.");
        }
    });
}

/// Resumes moves that were cut short by a quit or crash, once their drive is mounted. Moves
/// that failed with an error wait for the user (Resume or Undo), so a lasting problem is not
/// retried in a loop.
pub fn resume_all(e: &Shared) {
    let todo: Vec<(PathBuf, PathBuf, Job)> = {
        let eng = e.borrow();
        let jobs = jobs_root();
        FOLDERS
            .iter()
            .filter_map(|&d| home_path(d))
            .filter(|h| !eng.jobs.contains_key(h))
            .filter_map(|h| {
                let mount = if let Ok(target) = fs::read_link(&h) {
                    let i = eng.mounted_account(&target)?;
                    eng.cfg.accounts[i].mount.clone()
                } else {
                    // Crash window where the symlink is temporarily missing: recover mount from job.
                    let job = read_job(&jobs, &h)?;
                    let m = job.mount?;
                    let i = eng.mounted_account(&m)?;
                    eng.cfg.accounts[i].mount.clone()
                };
                match status(&h, &mount, &jobs) {
                    Status::Unfinished(job) if job.error.is_none() => Some((h, mount, job)),
                    _ => None,
                }
            })
            .collect()
    };
    for (home, mount, job) in todo {
        eprintln!("nuvolo: resuming the move of {}", home.display());
        run(e, home, mount, job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn queued_uploads_filters_to_moved_folder() {
        let v = json!({
            "queue": [
                { "name": "Documents/a.txt" },
                { "name": "Documents/sub/b.txt" },
                { "name": "DocumentsExtra/c.txt" },
                { "name": "Pictures/d.jpg" }
            ]
        });
        assert_eq!(queued_uploads_for(&v, "Documents"), 2);
        assert_eq!(queued_uploads_for(&v, "Pictures"), 1);
        assert_eq!(queued_uploads_for(&v, "Music"), 0);
    }
}

