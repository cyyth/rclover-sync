//! The Folders & Cache page: per-folder On-Demand / Offline / Excluded.

use super::widgets::*;
use super::Ui;
use crate::config::Mode;
use crate::errors::explain;
use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::json;
use std::rc::Rc;

impl Ui {
    pub(super) fn folders_page(self: &Rc<Self>) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();
        let g = group(
            "Cloud Folders",
            Some("Choose what stays on this device. On-Demand folders use no disk space until a file is opened. Offline folders also keep a copy that is there when Nuvolo is not running."),
        );
        let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
        reload.add_css_class("flat");
        reload.set_tooltip_text(Some("Reload folders"));
        let ui = self.clone();
        reload.connect_clicked(move |_| ui.load_folders());
        g.set_header_suffix(Some(&reload));
        g.add(&self.folders);
        page.add(&g);
        page
    }

    pub(super) fn load_folders(self: &Rc<Self>) {
        let job = {
            let e = self.e.borrow();
            e.client
                .clone()
                .and_then(|c| e.cfg.accounts.get(self.acct.get()).map(|a| (c, a.fs())))
        };
        self.folders.remove_all();
        let Some((client, fs_)) = job else {
            if self.e.borrow().rclone_starting {
                self.folders.append(&placeholder("Starting rclone…"));
            }
            return;
        };
        self.folders.append(&placeholder("Loading…"));
        let ui = self.clone();
        let acct = self.acct.get();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || client.call("operations/list", json!({ "fs": fs_, "remote": "", "opt": { "dirsOnly": true } })))
                .await
                .unwrap_or_else(|_| Err("listing crashed".to_owned()));
            if ui.acct.get() != acct {
                return;
            }
            ui.folders.remove_all();
            match res {
                Ok(v) => {
                    let names: Vec<String> = v["list"].as_array().into_iter().flatten().filter_map(|d| d["Name"].as_str().map(str::to_owned)).collect();
                    if names.is_empty() {
                        ui.folders.append(&placeholder("No folders at the top of this drive"));
                    }
                    for n in names {
                        ui.folders.append(&ui.folder_row(&n));
                    }
                }
                Err(e) => {
                    let row = placeholder("Could not list folders");
                    row.set_subtitle(&glib::markup_escape_text(&explain(&e)));
                    row.set_subtitle_lines(0);
                    ui.folders.append(&row);
                }
            }
        });
    }

    pub(super) fn folder_row(self: &Rc<Self>, name: &str) -> adw::ActionRow {
        let mode = self.e.borrow().cfg.accounts.get(self.acct.get()).map_or(Mode::OnDemand, |a| a.mode_of(name));
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(name).as_str()).subtitle(mode_label(mode)).build();
        row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
        let linked = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        linked.add_css_class("linked");
        linked.set_valign(gtk::Align::Center);
        let mut first: Option<gtk::ToggleButton> = None;
        for (m, text) in [(Mode::OnDemand, "On-Demand"), (Mode::Pinned, "Offline"), (Mode::Excluded, "Excluded")] {
            let b = gtk::ToggleButton::builder().label(text).active(m == mode).build();
            match &first {
                Some(f) => b.set_group(Some(f)),
                None => first = Some(b.clone()),
            }
            let (ui, name, row) = (self.clone(), name.to_owned(), row.clone());
            b.connect_toggled(move |b| {
                if !b.is_active() {
                    return;
                }
                let res = ui.e.borrow_mut().set_mode(ui.acct.get(), &name, m);
                ui.rebuild_home(); // the Home Folders rows show each folder's mode
                row.set_subtitle(mode_label(m));
                match res {
                    Ok(()) if m == Mode::Pinned => ui.toast(&format!("Downloading “{name}” to keep it offline")),
                    Ok(()) => {}
                    Err(e) => ui.toast(&format!("Saved; it applies after a remount ({e})")),
                }
            });
            linked.append(&b);
        }
        row.add_suffix(&linked);
        row
    }
}
