//! Two-way sync between a local folder and the cloud, used for offline copies. Slow (it
//! transfers files): run it off the main thread.

use super::Client;
use crate::config::ConflictPolicy;
use crate::errors::OFFLINE_EMPTIED;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::Duration;

/// Long enough for a first sync of a big folder.
const SYNC_TIMEOUT: Duration = Duration::from_secs(6 * 3600);

/// What to sync between the two folders.
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
mod tests {
    use super::*;

    #[test]
    fn finds_the_stale_lock_and_escapes_names() {
        let err = "prior lock file found: /w/x.lck\nTip: ... rclone deletefile \"/home/me/.cache/nuvolo/bisync/a..b.lck\" then run";
        assert_eq!(stale_lock(err).as_deref(), Some("/home/me/.cache/nuvolo/bisync/a..b.lck"));
        assert_eq!(stale_lock("bisync aborted"), None);
        assert_eq!(glob_escape("notes [draft].md"), "notes \\[draft\\].md");
    }
}
