//! Desktop notifications. They come in bulk and replace each other, never one per file.

use crate::engine::Engine;
use crate::errors::explain;
use gtk::gio;
use gtk::prelude::*;
use std::time::{Duration, Instant};

/// Shows a desktop notification. Notifications with the same `id` replace each other, so a
/// problem that repeats updates one notification instead of stacking new ones. Clicking it
/// opens the window on the page with the details.
pub fn notify(id: &str, title: &str, body: &str) {
    let Some(app) = gio::Application::default() else { return };
    let n = gio::Notification::new(title);
    n.set_body(Some(body));
    let page = if id == "errors" {
        "transfers"
    } else if id.starts_with("home-") {
        "prefs"
    } else {
        "overview"
    };
    n.set_default_action_and_target_value("app.show-page", Some(&page.to_variant()));
    app.send_notification(Some(id), &n);
}

/// Takes back a notification whose problem has gone away.
pub fn withdraw(id: &str) {
    if let Some(app) = gio::Application::default() {
        app.withdraw_notification(id);
    }
}

/// Turns rclone's running error count into at most one notification a minute
/// ("12 sync errors"), not one per file.
#[derive(Default)]
pub struct ErrorNotice {
    seen: u64,
    last: Option<Instant>,
}

impl ErrorNotice {
    pub fn check(&mut self, e: &Engine) {
        let total = e.core["errors"].as_u64().unwrap_or(0);
        self.seen = self.seen.min(total); // rclone restarted and counts from 0 again
        if total == self.seen || self.last.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return;
        }
        let n = total - self.seen;
        let last = e.core["lastError"].as_str().unwrap_or("unknown error");
        notify("errors", &format!("{n} sync error{}", if n == 1 { "" } else { "s" }), &format!("Latest: {}", explain(last)));
        (self.seen, self.last) = (total, Some(Instant::now()));
    }
}
