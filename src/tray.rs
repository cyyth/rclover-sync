//! Top-bar status icon (StatusNotifierItem, shown by GNOME's AppIndicator extension).
//!
//! ksni serves the D-Bus side on its own thread; menu clicks come back to the GTK main
//! thread through a channel.

use crate::engine::Engine;
use crate::APP_ID;
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};

pub type Handle = ksni::blocking::Handle<Tray>;

pub enum Cmd {
    Open,
    RetryMounts,
    Preferences,
    TogglePause,
    Quit,
}

pub struct Tray {
    pub status: String,
    pub paused: bool,
    /// Some account is not mounted: offer "Retry Mounting".
    pub failed: bool,
    tx: async_channel::Sender<Cmd>,
}

impl Tray {
    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send_blocking(cmd);
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "rclover".into()
    }

    fn icon_name(&self) -> String {
        format!("{APP_ID}-symbolic")
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: "rclover".into(), description: self.status.clone(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Cmd::Open);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem { label: self.status.clone(), enabled: false, ..Default::default() }.into(),
            MenuItem::Separator,
            StandardItem {
                label: "Retry Mounting".into(),
                visible: self.failed,
                activate: Box::new(|t: &mut Self| t.send(Cmd::RetryMounts)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.paused { "Resume Transfers" } else { "Pause Transfers" }.into(),
                activate: Box::new(|t: &mut Self| t.send(Cmd::TogglePause)),
                ..Default::default()
            }
            .into(),
            StandardItem { label: "Open rclover".into(), activate: Box::new(|t: &mut Self| t.send(Cmd::Open)), ..Default::default() }.into(),
            StandardItem { label: "Preferences".into(), activate: Box::new(|t: &mut Self| t.send(Cmd::Preferences)), ..Default::default() }.into(),
            MenuItem::Separator,
            StandardItem { label: "Quit".into(), activate: Box::new(|t: &mut Self| t.send(Cmd::Quit)), ..Default::default() }.into(),
        ]
    }
}

/// Starts the tray icon. Returns `None` when no StatusNotifier host is running.
pub fn spawn() -> (Option<Handle>, async_channel::Receiver<Cmd>) {
    let (tx, rx) = async_channel::unbounded();
    let tray = Tray { status: "Starting…".into(), paused: false, failed: false, tx };
    match tray.spawn() {
        Ok(handle) => (Some(handle), rx),
        Err(e) => {
            eprintln!("rclover: no tray icon ({e}); is the AppIndicator extension enabled?");
            (None, rx)
        }
    }
}

/// One-line status for the menu and tooltip, whether transfers are paused, and whether some
/// account is not mounted.
pub fn status(e: &Engine) -> (String, bool, bool) {
    let active = e.core["transferring"].as_array().map_or(0, Vec::len);
    let failed = e.failed_mounts();
    let status = if e.paused {
        "Paused".to_owned()
    } else if failed > 0 {
        format!("{failed} account{} not mounted", if failed == 1 { "" } else { "s" })
    } else if active > 0 {
        format!("Syncing {active} file{}", if active == 1 { "" } else { "s" })
    } else {
        "Up to date".to_owned()
    };
    (status, e.paused, failed > 0)
}
