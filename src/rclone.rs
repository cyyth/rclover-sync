//! The `rclone rcd` child process and a small client for its JSON remote-control API.

use crate::config::{Account, Config, Mode};
use base64::Engine as _;
use serde_json::{json, Value};
use std::io::{self, Read};
use std::path::Path;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Client {
    url: String,
    auth: String,
}

impl Client {
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let resp = ureq::post(&format!("{}/{method}", self.url))
            .timeout(Duration::from_secs(60))
            .set("Authorization", &self.auth)
            .send_json(params);
        match resp {
            Ok(r) => r.into_json().map_err(|e| e.to_string()),
            Err(ureq::Error::Status(_, r)) => Err(r
                .into_json::<Value>()
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("{method} failed"))),
            Err(e) => Err(e.to_string()),
        }
    }
}

pub struct Rcd {
    child: Child,
    pub client: Client,
}

impl Rcd {
    /// Starts `rclone rcd` on localhost with a random password (passed via the environment,
    /// so it never shows up in `ps`).
    pub fn spawn(port: u16, cache_dir: &Path) -> io::Result<Rcd> {
        let pass = random_token()?;
        let mut cmd = Command::new("rclone");
        cmd
            .arg("rcd")
            .arg("--rc-user=nuvolo")
            .arg(format!("--rc-addr=127.0.0.1:{port}"))
            .arg(format!("--cache-dir={}", cache_dir.display()))
            .env("RCLONE_RC_PASS", &pass)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // If Nuvolo dies, take rclone with it, so a crash never leaves the port taken.
        // SAFETY: prctl is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("nuvolo:{pass}"));
        let mut rcd = Rcd {
            child,
            client: Client { url: format!("http://127.0.0.1:{port}"), auth: format!("Basic {auth}") },
        };
        let start = Instant::now();
        while rcd.client.call("rc/noop", json!({})).is_err() {
            if let Some(status) = rcd.child.try_wait()? {
                return Err(io::Error::other(format!("rclone rcd exited ({status}); is port {port} already in use?")));
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(io::Error::other("rclone rcd did not answer within 10 s"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(rcd)
    }
}

impl Drop for Rcd {
    fn drop(&mut self) {
        let _ = self.client.call("mount/unmountall", json!({}));
        let _ = self.client.call("core/quit", json!({}));
        let start = Instant::now();
        while matches!(self.child.try_wait(), Ok(None)) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn random_token() -> io::Result<String> {
    let mut buf = [0u8; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Mounts an account with a full VFS cache. Ignore patterns and excluded folders become filter rules.
pub fn mount(c: &Client, cfg: &Config, a: &Account) -> Result<(), String> {
    std::fs::create_dir_all(&a.mount).map_err(|e| format!("{}: {e}", a.mount.display()))?;
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
        "mountOpt": { "VolumeName": a.remote },
        "_filter": { "ExcludeRule": excludes },
    });
    c.call("mount/mount", params.clone()).map(|_| ()).or_else(|first| {
        // A previous crash can leave a dead FUSE mount behind ("transport endpoint is not connected").
        let lazy = Command::new("fusermount3").arg("-uz").arg(&a.mount).status();
        if !matches!(lazy, Ok(s) if s.success()) {
            return Err(first);
        }
        c.call("mount/mount", params).map(|_| ())
    })
}

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
