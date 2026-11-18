//! The private `rclone rcd` child process.

use super::Client;
use base64::Engine as _;
use serde_json::json;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Rcd {
    child: Child,
    pub client: Client,
}

impl Rcd {
    /// Starts `rclone rcd` on localhost with a random password (passed via the environment,
    /// so it never shows up in `ps`). Waits up to 10 s for it to answer; needs no network.
    pub fn spawn(port: u16, cache_dir: &Path) -> io::Result<Rcd> {
        let pass = random_token()?;
        let mut cmd = Command::new("rclone");
        cmd.arg("rcd")
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
        let mut rcd = Rcd { child, client: Client { url: format!("http://127.0.0.1:{port}"), auth: format!("Basic {auth}") } };
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
        Ok(rcd)
    }

    /// False once the rclone process has exited.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
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

fn random_token() -> io::Result<String> {
    let mut buf = [0u8; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().fold(String::new(), |s, b| s + &format!("{b:02x}")))
}
