//! A small blocking client for rclone's remote-control API. Every call blocks until rclone
//! answers, so call it off the main thread unless the call is local and quick.

use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct Client {
    pub(super) url: String,
    pub(super) auth: String,
    pub(super) agent: ureq::Agent,
}

impl Client {
    pub(super) fn new(url: String, auth: String) -> Self {
        Client {
            url,
            auth,
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    /// Short timeout (3 s) for local rc calls (stats, health, control, config).
    pub fn call_local(&self, method: &str, params: Value) -> Result<Value, String> {
        self.call_for(method, params, Duration::from_secs(3))
    }

    /// Default timeout (60 s) for rc calls that may wait on the cloud.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.call_for(method, params, Duration::from_secs(60))
    }

    /// Like [`Client::call`], for jobs with an explicit timeout (e.g. mount, unmount, bisync).
    pub fn call_for(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let resp = self
            .agent
            .post(&format!("{}/{method}", self.url))
            .timeout(timeout)
            .set("Authorization", &self.auth)
            .send_json(params);
        match resp {
            Ok(r) => r.into_json().map_err(|e| e.to_string()),
            Err(ureq::Error::Status(_, r)) => Err(r
                .into_json::<Value>()
                .ok()
                .and_then(|v| v["error"].as_str().map(strip_ansi))
                .unwrap_or_else(|| format!("{method} failed"))),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Removes terminal colour codes ("\x1b[31m"), which rclone puts in some error texts.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip "[", parameters, and the final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_colour_codes() {
        assert_eq!(super::strip_ansi("\x1b[31mprior lock\x1b[0m found"), "prior lock found");
    }
}
