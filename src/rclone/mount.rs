//! Mounting a remote, and finding out what the kernel has mounted.

use super::{Client, RcloneCaps};
use crate::config::{Account, Config, Mode};
use crate::errors::{CLOUD_SILENT, NO_INTERNET};
use serde_json::json;
use std::collections::HashSet;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// Health of a FUSE mount point as seen by `stat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountHealth {
    Healthy,
    /// FUSE server is gone (`ENOTCONN`: "Transport endpoint is not connected").
    Dead,
    /// Mount returned `EIO` or did not answer within the probe timeout.
    Unhealthy,
}

/// Mounts an account with a full VFS cache. Slow (it reaches the cloud): call it off the main
/// thread. Ignore patterns and excluded folders become filter rules.
///
/// Opening a folder whose listing is not cached waits a round trip on the cloud (about 300 ms
/// on Google Drive), and GNOME Files opens every subfolder to count its items. A cloud that
/// reports its changes (Google Drive, OneDrive, Dropbox, ...) keeps listings correct through
/// polling, so they are kept for good and all of them are loaded once, in the background,
/// after mounting. Other clouds keep the 30-minute listing cache.
///
/// The mount point may hold files: the offline copies of pinned folders, which the mount
/// covers while it is up (see `engine::offline`).
pub fn mount(c: &Client, cfg: &Config, a: &Account, caps: RcloneCaps) -> Result<(), String> {
    // Without internet, rclone retries for minutes before mounting fails. Look the service
    // up first, which fails within seconds, so the user hears "no internet" right away.
    let kind = c.call_local("config/get", json!({ "name": a.remote }))?["type"].as_str().unwrap_or("").to_owned();
    if let Some(host) = service_host(&kind) {
        if (host, 443).to_socket_addrs().is_err() {
            return Err(NO_INTERNET.to_owned());
        }
    }
    let polls = c.call("operations/fsinfo", json!({ "fs": a.fs() })).is_ok_and(|v| v["Features"]["ChangeNotify"] == true);
    let mut excludes = cfg.ignore.clone();
    excludes.extend(a.modes.iter().filter(|(_, m)| **m == Mode::Excluded).map(|(p, _)| format!("/{p}/**")));
    // Divide the configured on-demand cache budget across accounts so N accounts do not use N × limit.
    let per_account_gib = (cfg.cache_max_size_gib / cfg.accounts.len().max(1) as u32).max(1);
    let mut vfs_opt = json!({
        "CacheMode": "full",
        "CacheMaxSize": format!("{per_account_gib}Gi"),
        "CacheMaxAge": format!("{}h", cfg.cache_max_age_hours),
        "WriteBack": "5s",
        "NoModTime": cfg.no_modtime,
        "DirCacheTime": if polls { "100000h" } else { "30m" },
        "PollInterval": "20s",
        "ReadAhead": "32Mi",
        "FastFingerprint": true,
    });
    if caps.has_min_free_space {
        vfs_opt["CacheMinFreeSpace"] = json!("1Gi");
    }
    let mount_point = canonical_mount_path(&a.mount);
    let params = json!({
        "fs": a.fs(),
        "mountPoint": mount_point,
        "vfsOpt": vfs_opt,
        "mountOpt": {
            "VolumeName": a.remote,
            "AllowNonEmpty": true,
            "DaemonTimeout": "2m",
        },
        "_filter": { "ExcludeRule": excludes },
    });
    c.call_for("mount/mount", params, Duration::from_secs(30)).map_err(|e| {
        // Our own wait ran out, not rclone: say so plainly instead of quoting the local request.
        if e.contains("127.0.0.1") { CLOUD_SILENT.to_owned() } else { e }
    })?;
    if polls {
        // Returns at once; rclone lists the whole drive as a background job.
        if let Err(e) = c.call_local("vfs/refresh", json!({ "fs": a.fs(), "recursive": "true", "_async": true })) {
            eprintln!("nuvolo: preloading folder listings of {}: {e}", a.remote);
        }
    }
    Ok(())
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

/// Resolves symlinks in the ancestor path of a mount point without `stat`-ing the mount point
/// itself (which would block on a wedged FUSE mount or fail on a dead one).
pub fn canonical_mount_path(p: &Path) -> PathBuf {
    if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
        if let Ok(real_parent) = std::fs::canonicalize(parent) {
            return real_parent.join(name);
        }
    }
    p.to_path_buf()
}

/// True if `p` (or its canonicalized path) is present in `/proc/self/mounts`.
pub fn is_mounted_path(mounted: &HashSet<PathBuf>, p: &Path) -> bool {
    mounted.contains(p) || mounted.contains(&canonical_mount_path(p))
}

/// Probes a FUSE mount point with a timeout so a wedged server cannot hang the caller.
pub fn probe_mount(p: &Path, timeout: Duration) -> MountHealth {
    let path = p.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let health = match std::fs::metadata(&path) {
            Ok(_) => MountHealth::Healthy,
            Err(e) if e.raw_os_error() == Some(libc::ENOTCONN) => MountHealth::Dead,
            Err(e) if e.raw_os_error() == Some(libc::EIO) => MountHealth::Unhealthy,
            Err(_) => MountHealth::Healthy,
        };
        let _ = tx.send(health);
    });
    rx.recv_timeout(timeout).unwrap_or(MountHealth::Unhealthy)
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

    #[test]
    fn canonical_mount_path_resolves_symlinked_parent() {
        let root = std::env::temp_dir().join(format!("nuvolo-canon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let real_cloud = root.join("real_cloud");
        std::fs::create_dir_all(&real_cloud).unwrap();
        let link_cloud = root.join("Cloud");
        std::os::unix::fs::symlink(&real_cloud, &link_cloud).unwrap();
        let via_link = link_cloud.join("gdrive");
        let expected = std::fs::canonicalize(&real_cloud).unwrap().join("gdrive");
        assert_eq!(canonical_mount_path(&via_link), expected);
        let mut mounted = HashSet::new();
        mounted.insert(expected);
        assert!(is_mounted_path(&mounted, &via_link));
        let _ = std::fs::remove_dir_all(root);
    }
}
