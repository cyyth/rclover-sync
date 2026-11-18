//! Two-way sync between a local folder and the cloud, used for offline copies.
//! Both calls are slow (they transfer files): run them off the main thread.

use super::Client;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

/// Long enough for a first sync of a big folder.
const SYNC_TIMEOUT: Duration = Duration::from_secs(6 * 3600);

/// Brings `local` and `remote` (`"gdrive:Music"`) to the same content. Changes on either side
/// win over an unchanged side; when both changed, the newer file wins and the other is kept
/// beside it with a `.conflict1` suffix, so nothing is lost. Deletions are passed on, within
/// bisync's safety limit (it refuses when more than half the files would go).
///
/// The first run merges both sides (the newer file wins) and passes on no deletions. Nuvolo
/// knows a pair has had its first run from a marker in `workdir`: bisync's error does not say.
pub fn bisync(c: &Client, local: &Path, remote: &str, workdir: &Path, excludes: &[String]) -> Result<(), String> {
    let marker = workdir.join("nuvolo").join(format!("{}..{remote}", local.display()).replace(['/', ':'], "_"));
    let first = !marker.exists();
    let mut params = json!({
        "path1": local,
        "path2": remote,
        "workdir": workdir,
        "compare": "size,modtime",
        "conflictResolve": "newer",
        "conflictLoser": "num",
        "createEmptySrcDirs": true,
        "resilient": true,
        "recover": true,
        "maxDelete": 50, // percent; the rc default of 0 refuses any deletion
        "_filter": { "ExcludeRule": excludes },
    });
    if first {
        params["resync"] = json!(true);
        params["resyncMode"] = json!("newer");
    }
    c.call_for("sync/bisync", params, SYNC_TIMEOUT)?;
    if first {
        std::fs::create_dir_all(workdir.join("nuvolo")).and_then(|_| std::fs::write(&marker, b"")).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Two-way sync for a single pinned file: the newer copy wins, in either direction.
/// A file missing on one side is copied from the other; deletions are not passed on.
pub fn sync_file(c: &Client, local: &Path, remote_fs: &str, rel: &str) -> Result<(), String> {
    let (Some(dir), Some(name)) = (local.parent(), local.file_name()) else {
        return Err(format!("{} is not a file path", local.display()));
    };
    let (dir, name) = (dir.to_string_lossy(), name.to_string_lossy());
    let copy = |src_fs: &str, src: &str, dst_fs: &str, dst: &str| -> Result<(), String> {
        // UpdateOlder (--update): skip when the destination is newer.
        let p: Value = json!({ "srcFs": src_fs, "srcRemote": src, "dstFs": dst_fs, "dstRemote": dst, "_config": { "UpdateOlder": true } });
        match c.call_for("operations/copyfile", p, SYNC_TIMEOUT) {
            Err(e) if e.contains("not found") => Ok(()), // nothing to copy from that side
            res => res.map(|_| ()),
        }
    };
    copy(&dir, &name, remote_fs, rel)?;
    copy(remote_fs, rel, &dir, &name)
}
