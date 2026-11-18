//! Mounting a remote, and finding out what the kernel has mounted.

use super::Client;
use crate::config::{Account, Config, Mode};
use crate::errors::{CLOUD_SILENT, NO_INTERNET};
use std::net::ToSocketAddrs;
use std::time::Duration;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Mounts an account with a full VFS cache. Slow (it reaches the cloud): call it off the main
/// thread. Ignore patterns and excluded folders become filter rules.
///
/// The mount point may hold files: the offline copies of pinned folders, which the mount
/// covers while it is up (see `engine::offline`).
pub fn mount(c: &Client, cfg: &Config, a: &Account) -> Result<(), String> {
    let mut excludes = cfg.ignore.clone();
    excludes.extend(a.modes.iter().filter(|(_, m)| **m == Mode::Excluded).map(|(p, _)| format!("/{p}/**")));
    let params = json!({
        "fs": a.fs(),
        "mountPoint": a.mount,
        "vfsOpt": {
            "CacheMode": "full",
            "CacheMaxSize": format!("{}Gi", cfg.cache_max_size_gib),
            "CacheMaxAge": format!("{}h", cfg.cache_max_age_hours),
            "WriteBack": "5s",
            "NoModTime": cfg.no_modtime,
        },
        "mountOpt": { "VolumeName": a.remote, "AllowNonEmpty": true },
        "_filter": { "ExcludeRule": excludes },
    });
    // Without internet, rclone retries for minutes before mounting fails. Look the service
    // up first, which fails within seconds, so the user hears "no internet" right away.
    let kind = c.call("config/get", json!({ "name": a.remote }))?["type"].as_str().unwrap_or("").to_owned();
    if let Some(host) = service_host(&kind) {
        if (host, 443).to_socket_addrs().is_err() {
            return Err(NO_INTERNET.to_owned());
        }
    }
    c.call_for("mount/mount", params, Duration::from_secs(30)).map(|_| ()).map_err(|e| {
        // Our own wait ran out, not rclone: say so plainly instead of quoting the local request.
        if e.contains("127.0.0.1") { CLOUD_SILENT.to_owned() } else { e }
    })
}

/// The API host of cloud services Nuvolo can check before mounting.
fn service_host(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "drive" => "www.googleapis.com",
        "onedrive" => "graph.microsoft.com",
        "dropbox" => "api.dropboxapi.com",
        "box" => "api.box.com",
        "pcloud" => "api.pcloud.com",
        _ => return None,
    })
}

/// True if `p` is a FUSE mount whose server is gone ("Transport endpoint is not connected").
pub fn is_dead_mount(p: &Path) -> bool {
    matches!(std::fs::metadata(p), Err(e) if e.raw_os_error() == Some(libc::ENOTCONN))
}

/// Detaches a mount point. Only for mounts no live rclone serves: when a live rclone later
/// releases a lazily detached mount, its cleanup unmounts whatever is mounted there then.
pub fn lazy_unmount(p: &Path) -> bool {
    Command::new("fusermount3").arg("-uz").arg(p).status().is_ok_and(|s| s.success())
}

/// Mount points currently listed in /proc/self/mounts. Cheap: never touches a mount.
pub fn mounted_paths() -> HashSet<PathBuf> {
    parse_mounts(&std::fs::read_to_string("/proc/self/mounts").unwrap_or_default())
}

fn parse_mounts(text: &str) -> HashSet<PathBuf> {
    text.lines()
        .filter_map(|l| l.split(' ').nth(1))
        // The kernel escapes spaces, tabs, newlines and backslashes as octal.
        .map(|p| PathBuf::from(p.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_escaped_mount_points() {
        let m = parse_mounts("gdrive: /home/me/Cloud/My\\040Drive fuse.rclone rw 0 0\nproc /proc proc rw 0 0\n");
        assert!(m.contains(Path::new("/home/me/Cloud/My Drive")));
        assert!(m.contains(Path::new("/proc")));
    }
}
