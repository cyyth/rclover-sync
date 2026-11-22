//! Cloud IDs: how an offline folder is recognised after it was renamed or moved. Clouds such
//! as Google Drive and OneDrive give every file and folder an ID that a rename keeps; others
//! (S3, SFTP, ...) have none, and there a renamed folder cannot be followed.

use super::Client;
use serde_json::json;
use std::time::Duration;

/// The ID of what the cloud has at `rel` in `fs_` (`"gdrive:"`): `None` if nothing is there,
/// `Some("")` on a cloud without IDs.
pub fn id_at(c: &Client, fs_: &str, rel: &str) -> Result<Option<String>, String> {
    let v = c.call("operations/stat", json!({ "fs": fs_, "remote": rel, "opt": { "showID": true, "noModTime": true, "noMimeType": true } }))?;
    Ok(v.get("item").filter(|i| !i.is_null()).map(|i| i["ID"].as_str().unwrap_or_default().to_owned()))
}

/// Where the item with `id`, last seen at `rel`, is now: first next to `rel` (a rename), then
/// among all folders of the drive (a move). `None` if it is not found: it was deleted.
// ponytail: a pinned single file moved into another folder is not found (only folders are
// searched drive-wide) and counts as deleted; its offline copy goes to the Trash. Upgrade path:
// search files too if that turns out to matter.
pub fn find_id(c: &Client, fs_: &str, rel: &str, id: &str) -> Result<Option<String>, String> {
    let parent = rel.rsplit_once('/').map_or("", |(p, _)| p);
    let list = |dir: &str, recurse: bool| {
        let opt = json!({ "showID": true, "recurse": recurse, "dirsOnly": recurse, "noModTime": true, "noMimeType": true });
        c.call_for("operations/list", json!({ "fs": fs_, "remote": dir, "opt": opt }), Duration::from_secs(600))
    };
    let find = |v: &serde_json::Value| v["list"].as_array()?.iter().find(|i| i["ID"].as_str() == Some(id))?["Path"].as_str().map(str::to_owned);
    // The parent may be gone too (it was renamed itself): then only the drive-wide search helps.
    if let Some(p) = list(parent, false).ok().as_ref().and_then(find) {
        return Ok(Some(p));
    }
    Ok(find(&list("", true)?))
}
