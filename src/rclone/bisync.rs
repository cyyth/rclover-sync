//! Keeping a local folder and the cloud the same, used for offline copies. Slow (it transfers
//! files): run it off the main thread.
//!
//! - [`bisync`]: two-way. Run once after each mount, to upload what changed while the drive
//!   was not mounted.
//! - [`mirror`]: one-way, cloud → local. While the drive is mounted every edit goes through
//!   the mount, so the offline copy only has to follow the cloud. A one-way copy cannot
//!   conflict and does not abort when a file changes mid-run: the next run fixes it.
//! - [`wait_for_uploads`]: waits until the mount has nothing left to upload.

use super::Client;
use crate::config::ConflictPolicy;
use crate::errors::OFFLINE_EMPTIED;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// Long enough for a first sync of a big folder.
const SYNC_TIMEOUT: Duration = Duration::from_secs(6 * 3600);

/// What to sync between the two folders.
#[derive(Clone, Copy)]
pub enum Scope<'a> {
    /// Everything, except these exclude rules.
    All(&'a [String]),
    /// Only these files directly in the folder (single pinned files), with their conflict
    /// copies.
    Files(&'a [String]),
}

/// Brings `local` and `remote` (`"gdrive:Music"`) to the same content. A change on one side
/// is copied to the other, deletions included. A file changed on both sides is settled by
/// `policy`; a kept loser is named `name.ext.conflict1` here (see `engine::offline` for the
/// readable name it gets afterwards).
///
/// Safety stops: bisync refuses when more than half the files would be deleted, or when every
/// file changed at once. `force` overrides them (the user's "Sync Anyway"). Single files are
/// always forced: for one file, "every file changed" is any change.
///
/// The first run merges both sides (the newer file wins) and passes on no deletions. Nuvolo
/// knows a pair had its first run from a marker in `workdir`: bisync's error does not say.
pub fn bisync(c: &Client, local: &Path, remote: &str, workdir: &Path, scope: Scope, policy: ConflictPolicy, force: bool) -> Result<(), String> {
    let own = workdir.join("nuvolo");
    fs::create_dir_all(&own).map_err(|e| e.to_string())?;
    let mut key = format!("{}..{remote}", local.display()).replace(['/', ':'], "_");
    let (resolve, loser) = match policy {
        ConflictPolicy::KeepBoth => ("newer", "num"),
        ConflictPolicy::Newer => ("newer", "delete"),
        ConflictPolicy::ThisDevice => ("path1", "delete"),
        ConflictPolicy::Cloud => ("path2", "delete"),
    };
    let mut params = json!({
        "path1": local,
        "path2": remote,
        "workdir": workdir,
        "compare": "size,modtime",
        "conflictResolve": resolve,
        "conflictLoser": loser,
        "createEmptySrcDirs": true,
        "resilient": true,
        "recover": true,
        "maxDelete": 50, // percent; the rc default of 0 refuses any deletion
        "force": force,
    });
    if matches!(scope, Scope::All(_)) && count_files(local) < SMALL_FOLDER {
        // In a small folder, half is a handful of files: deleting 1 of 2 is not a mass deletion.
        params["maxDelete"] = json!(100);
    }
    // Rules go in a filters file. Never `_filter`/`_config` here: in sync/bisync they change
    // more than they say (an include rule excluded everything; `_config` broke conflicts).
    let whole_folder = matches!(scope, Scope::All(_));
    let rules: String = match scope {
        Scope::All(excludes) => excludes.iter().fold(String::new(), |s, r| s + "- " + r + "\n"),
        Scope::Files(names) => {
            params["force"] = json!(true);
            // Each file, and its conflict copies ("name.conflict1"), and nothing else.
            names.iter().fold(String::new(), |s, n| {
                let n = glob_escape(n);
                s + "+ /" + &n + "\n+ /" + &n + ".conflict*\n"
            }) + "- **\n"
        }
    };
    if !rules.is_empty() {
        key.push_str(&format!("..{:x}", fnv(&rules)));
        let file = own.join(format!("{key}.filter"));
        fs::write(&file, rules).map_err(|e| e.to_string())?;
        params["filtersFile"] = json!(file);
    }
    // A new pair, or changed rules (bisync then demands a fresh start), merges first.
    let marker = own.join(key);
    let first = !marker.exists();
    // bisync will not sync from an empty folder (critical error, not even with force). An
    // offline copy emptied while Nuvolo was not running is put back from the cloud instead.
    let emptied = !first && whole_folder && fs::read_dir(local).map_or(true, |mut d| d.next().is_none());
    if first || emptied {
        params["resync"] = json!(true);
        params["resyncMode"] = json!("newer");
    }
    let mut res = c.call_for("sync/bisync", params.clone(), SYNC_TIMEOUT);
    if let Some(lock) = res.as_ref().err().and_then(|e| stale_lock(e)) {
        // Left by a sync that was cut off: Nuvolo runs one sync per account at a time.
        let _ = fs::remove_file(lock);
        res = c.call_for("sync/bisync", params, SYNC_TIMEOUT);
    }
    match res {
        Ok(_) if emptied => Err(OFFLINE_EMPTIED.to_owned()),
        Ok(_) => {
            if first {
                fs::write(&marker, b"").map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        Err(e) => {
            if e.contains("bisync aborted") {
                // bisync lost track of this pair; the next run starts it afresh (a merge,
                // which deletes nothing).
                let _ = fs::remove_file(&marker);
            }
            Err(e)
        }
    }
}

/// Below this many files a folder is too small for percentage-based deletion stops.
const SMALL_FOLDER: usize = 40;

/// Makes `local` the same as `remote` (`"gdrive:Music"`), one way: new and changed cloud files
/// are copied down, files gone from the cloud are deleted here. Never changes the cloud.
///
/// Safety stop (unless `force`): a whole folder that would lose more than half its files,
/// and more than [`SMALL_FOLDER`] / 2 of them, is left alone with a "too many deletes" error.
/// rclone's own `MaxDelete` cannot do this: it deletes up to the limit before it stops.
pub fn mirror(c: &Client, local: &Path, remote: &str, scope: Scope, force: bool) -> Result<(), String> {
    let rules: Vec<String> = match scope {
        Scope::All(excludes) => excludes.iter().map(|r| format!("- {r}")).collect(),
        Scope::Files(names) => names.iter().map(|n| format!("+ /{}", glob_escape(n))).chain(["- **".to_owned()]).collect(),
    };
    let filter = json!({ "FilterRule": rules });
    if !force && matches!(scope, Scope::All(_)) {
        let here = count_files(local);
        let there = c.call_for("operations/size", json!({ "fs": remote, "_filter": filter }), SYNC_TIMEOUT)?["count"].as_u64().unwrap_or(0) as usize;
        if too_many_deletes(here, there) {
            return Err(format!("too many deletes: {here} files here, {there} in the cloud"));
        }
    }
    let params = json!({
        "srcFs": remote,
        "dstFs": local,
        "createEmptySrcDirs": true,
        "_filter": filter,
    });
    c.call_for("sync/sync", params, SYNC_TIMEOUT).map(|_| ())
}

/// True if going from `here` files to `there` is a mass deletion.
fn too_many_deletes(here: usize, there: usize) -> bool {
    let gone = here.saturating_sub(there);
    gone * 2 > here && gone * 2 > SMALL_FOLDER
}

/// Waits until the mount of `fs_` has had no uploads waiting or running for a few seconds, so
/// a sync does not read the cloud while it is being written. Gives up after `max` (a sync
/// then still runs: a mirror fixes itself next time, a bisync may stop and retry later).
pub fn wait_for_uploads(c: &Client, fs_: &str, max: Duration) {
    let start = Instant::now();
    let mut quiet = 0;
    while quiet < 3 && start.elapsed() < max {
        match c.call_local("vfs/stats", json!({ "fs": fs_ })) {
            Ok(v) if crate::engine::uploads_in(&v) == 0 => quiet += 1,
            Ok(_) => quiet = 0,
            Err(_) => return, // not mounted (or rclone busy): nothing to wait for
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Files under `p`, not following links.
fn count_files(p: &Path) -> usize {
    fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => count_files(&e.path()),
            Ok(t) if t.is_file() => 1,
            _ => 0,
        })
        .sum()
}

/// The lock file named in bisync's "prior lock file found" error.
fn stale_lock(err: &str) -> Option<String> {
    const CMD: &str = "rclone deletefile \"";
    let rest = &err[err.find(CMD)? + CMD.len()..];
    Some(rest[..rest.find('"')?].to_owned())
}

/// Escapes rclone filter glob characters in a file name.
fn glob_escape(name: &str) -> String {
    name.chars().fold(String::new(), |mut s, c| {
        if "*?[]{}\\".contains(c) {
            s.push('\\');
        }
        s.push(c);
        s
    })
}

/// A short stable hash (FNV-1a), to tell rule sets apart in file names.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn finds_the_stale_lock_and_escapes_names() {
        let err = "prior lock file found: /w/x.lck\nTip: ... rclone deletefile \"/home/me/.cache/nuvolo/bisync/a..b.lck\" then run";
        assert_eq!(stale_lock(err).as_deref(), Some("/home/me/.cache/nuvolo/bisync/a..b.lck"));
        assert_eq!(stale_lock("bisync aborted"), None);
        assert_eq!(glob_escape("notes [draft].md"), "notes \\[draft\\].md");
    }

    #[test]
    fn mass_deletion_needs_half_and_more_than_a_handful() {
        assert!(!too_many_deletes(2, 1), "1 of 2 is not a mass deletion");
        assert!(!too_many_deletes(20, 0), "a small folder may be emptied");
        assert!(too_many_deletes(30, 0));
        assert!(!too_many_deletes(1000, 600));
        assert!(too_many_deletes(1000, 400));
        assert!(too_many_deletes(100, 0));
    }

    /// Runs `f` against a real `rclone rcd`, in a fresh temp folder; skipped without rclone.
    pub(crate) fn with_rclone(name: &str, f: impl FnOnce(&Client, &Path)) {
        if std::process::Command::new("rclone").arg("version").output().is_err() {
            return;
        }
        let d = std::env::temp_dir().join(format!("nuvolo-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let rcd = crate::rclone::Rcd::spawn(0, &d.join("cache")).expect("rclone starts");
        f(&rcd.client, &d);
        drop(rcd);
        let _ = fs::remove_dir_all(d);
    }

    fn put(p: &Path, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    #[test]
    fn mirror_follows_the_cloud_one_way() {
        with_rclone("mirror", |c, d| {
            let (cloud, here) = (d.join("cloud"), d.join("here"));
            for f in ["a.txt", "b [1].txt", "skip/x.bin", "sub/c.txt"] {
                put(&cloud.join(f), f);
            }
            fs::create_dir_all(&here).unwrap();
            let remote = cloud.to_string_lossy().into_owned();
            mirror(c, &here, &remote, Scope::All(&["/skip/**".to_owned()]), false).unwrap();
            assert_eq!(fs::read_to_string(here.join("sub/c.txt")).unwrap(), "sub/c.txt");
            assert!(!here.join("skip").exists(), "excluded");
            fs::remove_file(cloud.join("a.txt")).unwrap();
            put(&cloud.join("sub/c.txt"), "changed");
            mirror(c, &here, &remote, Scope::All(&[]), false).unwrap();
            assert!(!here.join("a.txt").exists(), "deleted in the cloud");
            assert_eq!(fs::read_to_string(here.join("sub/c.txt")).unwrap(), "changed");
            assert!(cloud.join("skip/x.bin").exists(), "the cloud is never changed");

            // Single files: only the named one is touched.
            let files = d.join("files");
            put(&files.join("local-only.txt"), "mine");
            mirror(c, &files, &remote, Scope::Files(&["b [1].txt".to_owned()]), true).unwrap();
            assert_eq!(fs::read_to_string(files.join("b [1].txt")).unwrap(), "b [1].txt");
            assert!(files.join("local-only.txt").exists());
            assert!(!files.join("sub").exists());

            // Mass deletion stops before deleting anything, unless forced.
            let (big, empty) = (d.join("big"), d.join("empty"));
            for n in 0..50 {
                put(&big.join(format!("{n}.txt")), "x");
            }
            fs::create_dir_all(&empty).unwrap();
            let err = mirror(c, &big, &empty.to_string_lossy(), Scope::All(&[]), false).unwrap_err();
            assert!(err.contains("too many deletes"), "{err}");
            assert_eq!(count_files(&big), 50);
            mirror(c, &big, &empty.to_string_lossy(), Scope::All(&[]), true).unwrap();
            assert_eq!(count_files(&big), 0);
        });
    }
}
