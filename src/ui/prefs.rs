//! The Preferences page: login startup, performance, home folders, OAuth and ignore patterns.

use super::widgets::*;
use super::{Ui, AGES};
use crate::config::ConflictPolicy;
use crate::errors::explain;
use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::json;
use std::rc::Rc;

impl Ui {
    pub(super) fn prefs_page(self: &Rc<Self>) -> adw::PreferencesPage {
        let cfg = self.e.borrow().cfg.clone();
        let page = adw::PreferencesPage::new();

        let g = group("General", None);
        let ui = self.clone();
        self.login.connect_active_notify(move |r| {
            if let Err(e) = set_autostart(r.is_active()) {
                ui.toast(&explain(&format!("Could not change login startup: {e}")));
            }
        });
        g.add(&self.login);
        page.add(&g);

        let g = group("Performance & API Limits", Some("Transfers and checkers apply immediately; cache settings apply after a remount."));
        let spin = |title: &str, sub: &str, max: f64, value: u32| {
            let r = adw::SpinRow::with_range(1.0, max, 1.0);
            r.set_title(title);
            r.set_subtitle(sub);
            r.set_value(value as f64);
            r
        };
        let transfers = spin("Concurrent Transfers", "--transfers", 32.0, cfg.transfers);
        let ui = self.clone();
        transfers.connect_value_notify(move |r| {
            let mut e = ui.e.borrow_mut();
            e.cfg.transfers = r.value() as u32;
            e.save();
            e.apply_options();
        });
        let checkers = spin("Checkers", "--checkers", 64.0, cfg.checkers);
        let ui = self.clone();
        checkers.connect_value_notify(move |r| {
            let mut e = ui.e.borrow_mut();
            e.cfg.checkers = r.value() as u32;
            e.save();
            e.apply_options();
        });
        let size = spin(
            "On-Demand Cache Size (GiB)",
            "Space for files opened on demand, on top of Offline folders. Files are cleared when it is full · --vfs-cache-max-size",
            2000.0,
            cfg.cache_max_size_gib,
        );
        let ui = self.clone();
        size.connect_value_notify(move |r| {
            {
                let mut e = ui.e.borrow_mut();
                e.cfg.cache_max_size_gib = r.value() as u32;
                e.save();
            }
            ui.toast_remount("Cache size saved");
        });
        let ages = gtk::StringList::new(&AGES.map(|(_, l)| l));
        let age = adw::ComboRow::builder().title("Cache Max Age").subtitle("--vfs-cache-max-age").model(&ages).build();
        age.set_selected(AGES.iter().position(|(h, _)| *h == cfg.cache_max_age_hours).unwrap_or(2) as u32);
        let ui = self.clone();
        age.connect_selected_notify(move |r| {
            {
                let mut e = ui.e.borrow_mut();
                e.cfg.cache_max_age_hours = AGES[r.selected() as usize].0;
                e.save();
            }
            ui.toast_remount("Cache age saved");
        });
        let modtime = adw::SwitchRow::builder()
            .title("Skip Modtimes")
            .subtitle("--no-modtime · fewer API calls on large trees")
            .active(cfg.no_modtime)
            .build();
        let ui = self.clone();
        modtime.connect_active_notify(move |r| {
            {
                let mut e = ui.e.borrow_mut();
                e.cfg.no_modtime = r.is_active();
                e.save();
            }
            ui.toast_remount("Saved");
        });
        for w in [transfers.upcast_ref::<gtk::Widget>(), checkers.upcast_ref(), size.upcast_ref(), age.upcast_ref(), modtime.upcast_ref()] {
            g.add(w);
        }
        page.add(&g);

        let g = group(
            "Home Folders",
            Some("Keep these folders in the cloud: the folder in your home becomes a link to its copy in the drive. What stays on this device follows the Folders & Cache page. Your current folder is kept as a backup."),
        );
        g.add(&self.home);
        page.add(&g);

        let g = group(
            "Conflicts",
            Some("When a file in an Offline folder changed both on this device and in the cloud since the last sync. Files edited through the mounted drive are saved straight to the cloud, so there the last save wins."),
        );
        let names = gtk::StringList::new(&ConflictPolicy::ALL.map(|p| p.label()));
        let policy = adw::ComboRow::builder().title("When a File Changed in Both Places").model(&names).build();
        policy.set_selected(ConflictPolicy::ALL.iter().position(|p| *p == cfg.conflicts).unwrap_or(0) as u32);
        policy.set_subtitle(&glib::markup_escape_text(cfg.conflicts.describe()));
        let ui = self.clone();
        policy.connect_selected_notify(move |r| {
            let p = ConflictPolicy::ALL[r.selected() as usize];
            r.set_subtitle(&glib::markup_escape_text(p.describe()));
            let mut e = ui.e.borrow_mut();
            e.cfg.conflicts = p;
            e.save();
        });
        g.add(&policy);
        page.add(&g);

        let g = group("OAuth & Security", None);
        let set = button("Set Up…");
        let ui = self.clone();
        set.connect_clicked(move |_| ui.client_id_dialog());
        self.client_row.add_suffix(&set);
        self.client_row.add_prefix(&gtk::Image::from_icon_name("dialog-password-symbolic"));
        g.add(&self.client_row);
        let reconnect = adw::ActionRow::builder().title("Reconnect Account").subtitle("Sign in again in your browser").build();
        reconnect.add_prefix(&gtk::Image::from_icon_name("view-refresh-symbolic"));
        let btn = button("Reconnect…");
        let ui = self.clone();
        btn.connect_clicked(move |b| ui.reconnect(b));
        reconnect.add_suffix(&btn);
        g.add(&reconnect);
        page.add(&g);

        let g = group("Ignore Patterns", Some("Glob patterns passed to rclone as exclude rules. Matching files are never uploaded or cached."));
        g.add(&self.ignore);
        let entry = adw::EntryRow::builder().title("Add a pattern, e.g. *.log").show_apply_button(true).build();
        entry.set_margin_top(12);
        let add_list = boxed();
        add_list.append(&entry);
        g.add(&add_list);
        let ui = self.clone();
        entry.connect_apply(move |row| {
            let p = row.text().trim().to_owned();
            if p.is_empty() {
                return;
            }
            {
                let mut e = ui.e.borrow_mut();
                if !e.cfg.ignore.contains(&p) {
                    e.cfg.ignore.push(p);
                }
                e.save();
            }
            row.set_text("");
            ui.rebuild_ignore();
            ui.toast_remount("Pattern added");
        });
        page.add(&g);
        self.rebuild_ignore();
        page
    }

    pub(super) fn rebuild_ignore(self: &Rc<Self>) {
        self.ignore.remove_all();
        for (i, p) in self.e.borrow().cfg.ignore.iter().enumerate() {
            let row = adw::ActionRow::builder().title(glib::markup_escape_text(p).as_str()).build();
            row.add_css_class("monospace");
            let del = gtk::Button::from_icon_name("user-trash-symbolic");
            del.add_css_class("flat");
            del.set_valign(gtk::Align::Center);
            del.set_tooltip_text(Some("Remove pattern"));
            let ui = self.clone();
            del.connect_clicked(move |_| {
                {
                    let mut e = ui.e.borrow_mut();
                    if i < e.cfg.ignore.len() {
                        e.cfg.ignore.remove(i);
                    }
                    e.save();
                }
                ui.rebuild_ignore();
                ui.toast_remount("Pattern removed");
            });
            row.add_suffix(&del);
            self.ignore.append(&row);
        }
    }

    pub(super) fn client_id_dialog(self: &Rc<Self>) {
        let Some(remote) = self.e.borrow().cfg.accounts.get(self.acct.get()).map(|a| a.remote.clone()) else { return };
        let d = adw::AlertDialog::new(
            Some("Custom OAuth Client"),
            Some("Create a Desktop OAuth client in Google Cloud Console and paste it here. Your own client gets its own API quota."),
        );
        let list = boxed();
        let id = adw::EntryRow::builder().title("Client ID").build();
        let secret = adw::PasswordEntryRow::builder().title("Client Secret").build();
        list.append(&id);
        list.append(&secret);
        d.set_extra_child(Some(&list));
        d.add_response("cancel", "Cancel");
        d.add_response("save", "Save");
        d.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        let ui = self.clone();
        d.connect_response(Some("save"), move |_, _| {
            let id = id.text().trim().to_owned();
            if !id.ends_with(".apps.googleusercontent.com") {
                return ui.toast("A Google client ID ends in .apps.googleusercontent.com");
            }
            let params = json!({
                "name": remote,
                "parameters": { "client_id": id, "client_secret": secret.text().trim() },
                "opt": { "nonInteractive": true },
            });
            match ui.e.borrow().client.call("config/update", params) {
                Ok(_) => ui.toast("Saved. Reconnect the account to start using it."),
                Err(e) => ui.toast(&e),
            }
            ui.select_account(ui.acct.get());
        });
        d.present(Some(&self.window));
    }

    pub(super) fn reconnect(self: &Rc<Self>, btn: &gtk::Button) {
        let i = self.acct.get();
        let Some(remote) = self.e.borrow().cfg.accounts.get(i).map(|a| a.remote.clone()) else { return };
        // Disabled until sign-in finishes, so a second click cannot start a second sign-in.
        btn.set_sensitive(false);
        btn.set_label("Waiting for Sign-In…");
        self.toast("Opening your browser to sign in…");
        let (ui, btn) = (self.clone(), btn.clone());
        glib::spawn_future_local(async move {
            let ok = gio::spawn_blocking(move || {
                std::process::Command::new("rclone")
                    .args(["config", "reconnect", &format!("{remote}:"), "--auto-confirm"])
                    .status()
                    .is_ok_and(|s| s.success())
            })
            .await
            .unwrap_or(false);
            btn.set_label("Reconnect…");
            btn.set_sensitive(true);
            if !ok {
                return ui.toast("Sign-in did not finish. Click Reconnect again and complete the sign-in in your browser.");
            }
            let res = ui.e.borrow_mut().remount(i);
            ui.toast(&res.map_or_else(|e| format!("Reconnected; remount later ({})", explain(&e)), |_| "Account reconnected".to_owned()));
        });
    }
}

pub(super) fn autostart_file() -> std::path::PathBuf {
    glib::user_config_dir().join("autostart").join(format!("{}.desktop", crate::APP_ID))
}

/// Adds or removes an XDG autostart entry that starts Nuvolo as a background service.
fn set_autostart(on: bool) -> std::io::Result<()> {
    let path = autostart_file();
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe()?;
    std::fs::create_dir_all(path.parent().expect("autostart dir"))?;
    std::fs::write(
        &path,
        format!(
            "[Desktop Entry]\nType=Application\nName=Nuvolo\nComment=Mount cloud drives at login\nExec={} --gapplication-service\nIcon={}\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            exe.display(),
            crate::APP_ID
        ),
    )
}
