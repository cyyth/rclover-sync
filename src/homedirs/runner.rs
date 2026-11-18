//! Running home-folder moves in the background, and resuming interrupted ones. Works without
//! the window: progress is in `Engine::jobs`, outcomes are notifications.

use super::{clear_job, finish, home_path, jobs_root, status, write_job, Job, Op, Status, FOLDERS};
use crate::engine::{Progress, Shared};
use gtk::{gio, glib};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
            // The files are in the cache; wait until rclone has uploaded them. Ask rclone each
            // time: the stats from the last tick may predate the copy and show nothing pending.
            let target = e.borrow().locate(&mount).map(|(i, _)| (e.borrow().client.clone(), e.borrow().cfg.accounts[i].fs()));
            while let Some((client, fs_)) = target.clone() {
                let left = gio::spawn_blocking(move || client.call("vfs/stats", serde_json::json!({ "fs": fs_ })).map(|v| crate::engine::uploads_in(&v)))
                    .await
                    .ok()
                    .and_then(Result::ok);
                if left == Some(0) {
                    break;
                }
                if let Some(p) = e.borrow_mut().jobs.get_mut(&home) {
                    p.uploading = true;
                    p.count.store(left.unwrap_or(0) as usize, Ordering::Relaxed);
                }
                crate::ui::refresh_home();
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
        FOLDERS
            .iter()
            .filter_map(|&d| home_path(d))
            .filter(|h| !eng.jobs.contains_key(h))
            .filter_map(|h| {
                let target = fs::read_link(&h).ok()?;
                let i = eng.mounted_account(&target)?;
                let mount = eng.cfg.accounts[i].mount.clone();
                match status(&h, &mount, &jobs_root()) {
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

