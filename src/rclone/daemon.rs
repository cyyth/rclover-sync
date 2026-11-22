//! The private `rclone rcd` child process.

use super::Client;
use base64::Engine as _;
use serde_json::json;
use std::io::{self, Read};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Minimum rclone version required for bisync conflict/recovery options.
const MIN_RCLONE: (u32, u32, u32) = (1, 66, 0);

/// Feature flags detected from the running rclone version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RcloneCaps {
    /// `--vfs-cache-min-free-space` / `vfsOpt.CacheMinFreeSpace` (rclone >= 1.67).
    pub has_min_free_space: bool,
}

pub struct Rcd {
    child: Child,
    pub client: Client,
    pub caps: RcloneCaps,
}

impl Rcd {
    /// Starts `rclone rcd` on localhost with a random password (passed via the environment,
    /// so it never shows up in `ps`). Falls back to a free ephemeral port if `preferred_port`
    /// is occupied. Waits up to 10 s for it to answer; needs no network.
    pub fn spawn(preferred_port: u16, cache_dir: &Path) -> io::Result<Rcd> {
        std::fs::create_dir_all(cache_dir)?;
        let port = pick_port(preferred_port)?;
        match Self::spawn_on(port, cache_dir) {
            Ok(rcd) => Ok(rcd),
            Err(_) if port == preferred_port => {
                // Race on the preferred port: retry once on an OS-assigned free port.
                let fallback = pick_free_port()?;
                eprintln!("rclover: port {preferred_port} was busy, retrying rclone on port {fallback}");
                Self::spawn_on(fallback, cache_dir)
            }
            Err(e) => Err(e),
        }
    }

    fn spawn_on(port: u16, cache_dir: &Path) -> io::Result<Rcd> {
        let pass = random_token()?;
        let log_file = cache_dir.join("rclone.log");
        let mut cmd = Command::new("rclone");
        cmd.arg("rcd")
            .arg("--rc-user=rclover")
            .arg(format!("--rc-addr=127.0.0.1:{port}"))
            .arg(format!("--cache-dir={}", cache_dir.display()))
            .arg(format!("--log-file={}", log_file.display()))
            .arg("--log-level=ERROR")
            .env("RCLONE_RC_PASS", &pass)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // If rclover dies, take rclone with it, so a crash never leaves the port taken.
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
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("rclover:{pass}"));
        let client = Client::new(format!("http://127.0.0.1:{port}"), format!("Basic {auth}"));
        let mut rcd = Rcd { child, client, caps: RcloneCaps::default() };
        let start = Instant::now();
        while rcd.client.call_for("rc/noop", json!({}), Duration::from_secs(2)).is_err() {
            if let Some(status) = rcd.child.try_wait()? {
                return Err(io::Error::other(format!("rclone rcd exited ({status}); is port {port} already in use?")));
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(io::Error::other("rclone rcd did not answer within 10 s"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Ok(v) = rcd.client.call_local("core/version", json!({})) {
            if let Some(ver_str) = v["version"].as_str() {
                if let Some(ver) = parse_version(ver_str) {
                    if ver < MIN_RCLONE {
                        return Err(io::Error::other(format!(
                            "rclone {ver_str} is too old; rclover requires rclone {}.{}.{} or newer",
                            MIN_RCLONE.0, MIN_RCLONE.1, MIN_RCLONE.2
                        )));
                    }
                    rcd.caps = RcloneCaps {
                        has_min_free_space: ver >= (1, 67, 0),
                    };
                }
            }
        }
        Ok(rcd)
    }

    /// False once the rclone process has exited.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Drops `old` (if any) and spawns a new `rclone rcd` on a dedicated long-lived spawner
    /// thread, then passes the result to `done`.
    ///
    /// Using one persistent thread (instead of a short-lived `std::thread::spawn`) is required
    /// on Linux because `PR_SET_PDEATHSIG` fires when the *spawning thread* exits, not when the
    /// process exits.
    pub fn spawn_async(old: Option<Rcd>, preferred_port: u16, cache_dir: PathBuf, done: impl FnOnce(Result<Rcd, String>) + Send + 'static) {
        static SPAWNER: OnceLock<Sender<Box<dyn FnOnce() + Send>>> = OnceLock::new();
        let tx = SPAWNER.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
            std::thread::Builder::new()
                .name("rclone-spawner".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        job();
                    }
                })
                .expect("spawn rclone-spawner thread");
            tx
        });
        let _ = tx.send(Box::new(move || {
            drop(old);
            let res = Rcd::spawn(preferred_port, &cache_dir).map_err(|e| format!("could not start rclone: {e}"));
            done(res);
        }));
    }
}

impl Drop for Rcd {
    fn drop(&mut self) {
        let _ = self.client.call_for("mount/unmountall", json!({}), Duration::from_secs(10));
        let _ = self.client.call_for("core/quit", json!({}), Duration::from_secs(2));
        let start = Instant::now();
        while matches!(self.child.try_wait(), Ok(None)) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pick_port(preferred: u16) -> io::Result<u16> {
    if preferred != 0 && TcpListener::bind(("127.0.0.1", preferred)).is_ok() {
        return Ok(preferred);
    }
    let free = pick_free_port()?;
    if preferred != 0 {
        eprintln!("rclover: port {preferred} is in use, using port {free} for rclone");
    }
    Ok(free)
}

fn pick_free_port() -> io::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// Parses an rclone version string like `"v1.75.2"` or `"v1.66.0-beta.1"` into `(major, minor, patch)`.
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let core = s.trim().trim_start_matches('v').split('-').next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

fn random_token() -> io::Result<String> {
    let mut buf = [0u8; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().fold(String::new(), |s, b| s + &format!("{b:02x}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rclone_versions() {
        assert_eq!(parse_version("v1.75.2"), Some((1, 75, 2)));
        assert_eq!(parse_version("v1.66.0-beta.1234"), Some((1, 66, 0)));
        assert_eq!(parse_version("1.67"), Some((1, 67, 0)));
        assert_eq!(parse_version("unknown"), None);
    }

    #[test]
    fn spawn_async_keeps_rclone_alive() {
        // Skip in minimal build containers where rclone is not installed.
        if Command::new("rclone").arg("version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rclover-rcd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, rx) = mpsc::channel();
        Rcd::spawn_async(None, 0, dir.clone(), move |res| {
            let _ = tx.send(res);
        });
        let mut rcd = rx.recv_timeout(Duration::from_secs(12)).expect("spawn_async replied").expect("rclone started");
        // Previously, PR_SET_PDEATHSIG killed rclone as soon as the short-lived spawn thread exited.
        std::thread::sleep(Duration::from_millis(300));
        assert!(rcd.alive(), "rclone must stay alive after spawn_async completes");
        rcd.client
            .call_local("options/set", json!({ "main": { "Transfers": 4, "Checkers": 8, "BufferSize": 33554432u64 } }))
            .expect("options/set with BufferSize succeeds");
        drop(rcd);
        let _ = std::fs::remove_dir_all(dir);
    }
}
