//! A small blocking client for rclone's remote-control API. Every call blocks until rclone
//! answers, so call it off the main thread unless the call is local and quick.

use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct Client {
    pub(super) url: String,
    pub(super) auth: String,
}

impl Client {
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.call_for(method, params, Duration::from_secs(60))
    }

    /// Like [`Client::call`], for jobs that legitimately run long (a two-way sync).
    pub fn call_for(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let resp = ureq::post(&format!("{}/{method}", self.url))
            .timeout(timeout)
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
