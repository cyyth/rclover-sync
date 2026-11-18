//! Top-bar status icon (StatusNotifierItem, shown by GNOME's AppIndicator extension).
//!
//! ksni serves the D-Bus side on its own thread; menu clicks come back to the GTK main
//! thread through a channel.

use crate::APP_ID;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};

pub enum Cmd {
    Open,
    Preferences,
    TogglePause,
    Quit,
}

pub struct Tray {
    pub status: String,
    pub paused: bool,
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
        "Nuvolo".into()
    }

    fn icon_name(&self) -> String {
        format!("{APP_ID}-symbolic")
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: "Nuvolo".into(), description: self.status.clone(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Cmd::Open);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem { label: self.status.clone(), enabled: false, ..Default::default() }.into(),
            MenuItem::Separator,
            StandardItem {
                label: if self.paused { "Resume Transfers" } else { "Pause Transfers" }.into(),
                activate: Box::new(|t: &mut Self| t.send(Cmd::TogglePause)),
                ..Default::default()
            }
            .into(),
            StandardItem { label: "Open Nuvolo".into(), activate: Box::new(|t: &mut Self| t.send(Cmd::Open)), ..Default::default() }.into(),
            StandardItem { label: "Preferences".into(), activate: Box::new(|t: &mut Self| t.send(Cmd::Preferences)), ..Default::default() }.into(),
            MenuItem::Separator,
            StandardItem { label: "Quit".into(), activate: Box::new(|t: &mut Self| t.send(Cmd::Quit)), ..Default::default() }.into(),
        ]
    }
}

/// Starts the tray icon. Returns `None` when no StatusNotifier host is running.
pub fn spawn() -> (Option<Handle<Tray>>, async_channel::Receiver<Cmd>) {
    let (tx, rx) = async_channel::unbounded();
    let tray = Tray { status: "Starting…".into(), paused: false, tx };
    match tray.spawn() {
        Ok(handle) => (Some(handle), rx),
        Err(e) => {
            eprintln!("nuvolo: no tray icon ({e}); is the AppIndicator extension enabled?");
            (None, rx)
        }
    }
}
