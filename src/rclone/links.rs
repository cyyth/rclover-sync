//! Share links and browser URLs for files in a remote.

use super::Client;
use serde_json::json;

pub fn public_link(c: &Client, remote: &str, rel: &str) -> Result<String, String> {
    let v = c.call("operations/publiclink", json!({ "fs": format!("{remote}:"), "remote": rel }))?;
    v["url"].as_str().map(str::to_owned).ok_or_else(|| "the backend returned no link".to_owned())
}

/// A browser URL for the file. Google Drive gets a direct link; other backends fall back to a share link.
pub fn web_url(c: &Client, remote: &str, rel: &str) -> Result<String, String> {
    let kind = c.call("config/get", json!({ "name": remote }))?["type"].as_str().unwrap_or("").to_owned();
    if kind != "drive" {
        return public_link(c, remote, rel);
    }
    let v = c.call("operations/stat", json!({ "fs": format!("{remote}:"), "remote": rel, "opt": {} }))?;
    let id = v["item"]["ID"].as_str().ok_or_else(|| "Google Drive returned no file ID".to_owned())?;
    Ok(if v["item"]["IsDir"].as_bool().unwrap_or(false) {
        format!("https://drive.google.com/drive/folders/{id}")
    } else {
        format!("https://drive.google.com/file/d/{id}/view")
    })
}
