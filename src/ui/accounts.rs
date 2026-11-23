//! The account sidebar: listing, selecting and adding accounts.

use super::widgets::*;
use super::Ui;
use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{json, Value};
use std::rc::Rc;

impl Ui {
    pub fn add_account(self: &Rc<Self>) {
        let Some(client) = self.e.borrow().client.clone() else {
            return self.toast("rclone is still starting; try again in a moment.");
        };
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let v = gio::spawn_blocking(move || client.call_local("config/listremotes", json!({})))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(Value::Null);
            let remotes: Vec<String> = v["remotes"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_owned)).collect();
            let have: Vec<String> = ui.e.borrow().cfg.accounts.iter().map(|a| a.remote.clone()).collect();
            let avail: Vec<String> = remotes.into_iter().filter(|r| !have.contains(r)).collect();
            if avail.is_empty() {
                let d = adw::AlertDialog::new(
                    Some("No Remotes to Add"),
                    Some("Create a remote first by running “rclone config” in a terminal, then try again."),
                );
                d.add_response("ok", "OK");
                d.present(Some(&ui.window));
                return;
            }
            let d = adw::AlertDialog::new(Some("Add Cloud Account"), Some("Choose an rclone remote. It will be mounted under ~/Cloud."));
            d.add_response("cancel", "Cancel");
            let list = boxed();
            for r in &avail {
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(r).as_str()).activatable(true).build();
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                list.append(&row);
            }
            d.set_extra_child(Some(&list));
            let (ui2, dlg) = (ui.clone(), d.clone());
            list.connect_row_activated(move |_, row| {
                let i = ui2.e.borrow_mut().add_account(&avail[row.index() as usize]);
                dlg.close();
                ui2.rebuild_accounts();
                ui2.select_account(i);
            });
            d.present(Some(&ui.window));
        });
    }

    pub(super) fn rebuild_accounts(self: &Rc<Self>) {
        *self.acct_summary.borrow_mut() = Vec::new();
        self.tick_accounts_sidebar();
    }

    pub(super) fn tick_accounts_sidebar(self: &Rc<Self>) {
        let summary: Vec<(String, String, &'static str, bool)> = {
            let e = self.e.borrow();
            let cur = self.acct.get();
            e.cfg
                .accounts
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let (sub, kind) = match e.mount_state(i) {
                        crate::engine::MountState::Mounting => ("Mounting…".to_owned(), "warning"),
                        crate::engine::MountState::Failed(err) => (
                            if crate::errors::is_offline(&err) { "Offline".to_owned() } else { "Not Mounted".to_owned() },
                            "error",
                        ),
                        crate::engine::MountState::Mounted if e.paused => ("Paused".to_owned(), "warning"),
                        crate::engine::MountState::Mounted => (
                            if a.on_demand() { "Mounted · On-Demand".to_owned() } else { "Mounted · Offline".to_owned() },
                            "success",
                        ),
                    };
                    (a.remote.clone(), sub, kind, i == cur)
                })
                .collect()
        };
        if *self.acct_summary.borrow() == summary && self.accounts.first_child().is_some() {
            return;
        }
        *self.acct_summary.borrow_mut() = summary.clone();
        self.accounts.remove_all();
        for (remote, sub, kind, is_current) in &summary {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.append(&adw::Avatar::new(32, Some(remote), true));
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            text.set_hexpand(true);
            let title_classes: &[&str] = if *is_current { &["heading"] } else { &[] };
            text.append(&label(remote, title_classes));
            let sub_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let dot = label("●", &["status-dot", kind]);
            sub_box.append(&dot);
            sub_box.append(&label(sub, &["caption", "dim-label"]));
            text.append(&sub_box);
            row.append(&text);
            if *is_current {
                let chk = gtk::Image::from_icon_name("object-select-symbolic");
                chk.add_css_class("accent");
                row.append(&chk);
            }
            self.accounts.append(&row);
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&gtk::Image::from_icon_name("list-add-symbolic"));
        row.append(&label("Add Cloud Account", &["accent"]));
        self.accounts.append(&row);
    }

    pub(super) fn select_account(self: &Rc<Self>, i: usize) {
        self.acct.set(i);
        self.tick_accounts_sidebar();
        let (remote, mount, bg, on_demand, dismissed) = {
            let e = self.e.borrow();
            let Some(a) = e.cfg.accounts.get(i) else {
                self.title.set_subtitle("No account yet");
                self.ov.mount_row.set_subtitle("Add an rclone remote to get started");
                self.banner.set_revealed(false);
                return;
            };
            (
                a.remote.clone(),
                a.mount.to_string_lossy().into_owned(),
                a.background_sync,
                a.on_demand(),
                a.oauth_banner_dismissed,
            )
        };
        self.title.set_subtitle(&remote);
        self.ov.mount_row.set_subtitle(&glib::markup_escape_text(&mount));
        self.updating.set(true);
        self.ov.ondemand.set_active(on_demand);
        self.ov.bgsync.set_active(bg);
        self.updating.set(false);
        if let Some(client) = self.e.borrow().client.clone() {
            let ui = self.clone();
            glib::spawn_future_local(async move {
                let info = gio::spawn_blocking(move || client.call_local("config/get", json!({ "name": remote }))).await.ok().and_then(Result::ok).unwrap_or(Value::Null);
                if ui.acct.get() != i {
                    return;
                }
                let client_id = info["client_id"].as_str().unwrap_or("");
                ui.banner.set_revealed(info["type"] == "drive" && client_id.is_empty() && !dismissed);
                ui.client_row.set_subtitle(if client_id.is_empty() { "rclone shared client (rate-limited)" } else { client_id });
            });
        }
        *self.shown.borrow_mut() = (u64::MAX, String::new());
        self.load_folders();
        self.rebuild_home();
        self.tick();
    }

    pub(super) fn open_mount(&self) {
        let Some(mount) = self.e.borrow().cfg.accounts.get(self.acct.get()).map(|a| a.mount.clone()) else { return };
        let uri = gio::File::for_path(mount).uri();
        if let Err(err) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
            self.toast(&err.to_string());
        }
    }
}
