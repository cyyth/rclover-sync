//! Home Folders rows (Preferences): moving Documents, Music, ... to the cloud and back,
//! and resuming or undoing a move that did not finish. The work itself is in `homedirs`.

use super::widgets::*;
use super::Ui;
use crate::config::Mode;
use crate::errors::explain;
use crate::homedirs::{self, Job, Op, Status};
use adw::prelude::*;
use gtk::glib;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::Ordering;

impl Ui {
    /// Rows for the XDG folders, from what is on disk (links and job files), so a move that
    /// failed or was cut short shows as such, with Resume and Undo, after a restart too.
    ///
    /// A row is only the link. How the cloud folder is kept (On-Demand, Offline, Excluded, or
    /// a mix inside it) is set on the Folders & Cache page, and each row just shows it. An
    /// Excluded folder is hidden from the drive, so its row is greyed out.
    pub(super) fn rebuild_home(self: &Rc<Self>) {
        self.home.remove_all();
        let Some(mount) = self.e.borrow().cfg.accounts.get(self.acct.get()).map(|a| a.mount.clone()) else {
            self.home.append(&placeholder("Add a cloud account first"));
            return;
        };
        let jobs = homedirs::jobs_root();
        for dir in homedirs::FOLDERS {
            let Some(home) = homedirs::home_path(dir) else { continue };
            let name = home.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let cloud = homedirs::cloud_path(&mount, &home);
            let status = homedirs::status(&home, &mount, &jobs);
            let (mode, mixed) = self.e.borrow().cfg.accounts.get(self.acct.get()).map_or((Mode::OnDemand, false), |a| (a.mode_of(&name), a.is_mixed(&name)));
            let kept = match mode {
                Mode::OnDemand => "On-Demand: files download when opened, so the link needs Nuvolo running",
                Mode::Pinned => "Offline: kept on this device, also when Nuvolo is not running",
                Mode::Excluded => "",
            };
            let kept = if mixed { format!("{kept}. Some folders inside are set differently.") } else { format!("{kept}.") };
            let excluded = format!(
                "Excluded on the Folders & Cache page, so the drive hides {}. Set it to On-Demand or Offline there to use it.",
                cloud.display()
            );
            let row = adw::ActionRow::builder().title(glib::markup_escape_text(&name).as_str()).subtitle_lines(0).build();
            let sub = |t: &str| row.set_subtitle(&glib::markup_escape_text(t));
            let icon = match &status {
                Status::Local => "folder-symbolic",
                Status::Linked | Status::Elsewhere(_) => "folder-remote-symbolic",
                Status::Unfinished(j) if j.error.is_some() => "dialog-warning-symbolic",
                Status::Unfinished(_) => "folder-download-symbolic",
            };
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            if let Some(p) = self.e.borrow().jobs.get(&home) {
                // Running: a spinner and a live count instead of buttons, so it cannot start twice.
                let n = p.count.load(Ordering::Relaxed);
                sub(&match (p.op, p.uploading) {
                    (Op::Link, false) => format!("Copying to the cloud… {n} files so far"),
                    (Op::Link, true) => format!("Uploading… {n} files left. Your files are already in {}.", cloud.display()),
                    (Op::Unlink, _) => format!("Downloading to this device… {n} files so far"),
                });
                row.add_suffix(&gtk::Spinner::builder().spinning(true).valign(gtk::Align::Center).build());
                self.home.append(&row);
                continue;
            }
            match status {
                Status::Local | Status::Linked if mode == Mode::Excluded => {
                    sub(&excluded);
                    row.set_sensitive(false);
                }
                Status::Local => {
                    sub(&format!("On this device ({}). Its cloud folder is set to {kept}", home.display()));
                    self.home_button(&row, "Move to Cloud", false, {
                        let (ui, home, mount) = (self.clone(), home.clone(), mount.clone());
                        move || ui.link_home(&home, &mount)
                    });
                }
                Status::Linked => {
                    sub(&format!("Linked to {} · {kept}", cloud.display()));
                    self.home_button(&row, "Move Back", false, {
                        let (ui, home, mount) = (self.clone(), home.clone(), mount.clone());
                        move || ui.unlink_home(&home, &mount)
                    });
                }
                Status::Elsewhere(t) => sub(&format!("Linked to {}", t.display())),
                Status::Unfinished(job) => {
                    let why = job.error.as_deref().map_or_else(|| "It was interrupted and resumes once the drive is mounted.".to_owned(), explain);
                    sub(&match job.op {
                        Op::Link => format!(
                            "Not fully moved to the cloud: {why}\nNothing is lost: your original folder is in {}.",
                            job.backup.as_deref().unwrap_or(Path::new("?")).display()
                        ),
                        Op::Unlink => format!("Not fully moved back: {why}\nThe cloud copy is untouched."),
                    });
                    self.home_button(&row, if job.op == Op::Link { "Undo" } else { "Cancel" }, false, {
                        let (ui, home, mount) = (self.clone(), home.clone(), mount.clone());
                        move || ui.undo_home(&home, &mount)
                    });
                    self.home_button(&row, "Resume", true, {
                        let (ui, home, mount) = (self.clone(), home.clone(), mount.clone());
                        move || ui.start_home(home.clone(), mount.clone(), Job { error: None, ..job.clone() })
                    });
                }
            }
            self.home.append(&row);
        }
    }

    pub(super) fn home_button(&self, row: &adw::ActionRow, text: &str, suggested: bool, on_click: impl Fn() + 'static) {
        let b = button(text);
        if suggested {
            b.add_css_class("suggested-action");
        }
        b.connect_clicked(move |b| {
            b.set_sensitive(false); // until the dialog is answered or the row is redrawn
            on_click();
        });
        row.add_suffix(&b);
    }

    /// Starts or resumes a move once the drive is known to be mounted.
    pub(super) fn start_home(self: &Rc<Self>, home: PathBuf, mount: PathBuf, job: homedirs::Job) {
        if self.e.borrow().mounted_account(&mount).is_none() {
            self.rebuild_home();
            return self.toast("The drive is not mounted yet. Nuvolo resumes by itself once it is; see Overview for why it is not mounted.");
        }
        if let Err(e) = homedirs::write_job(&homedirs::jobs_root(), &home, &job) {
            self.rebuild_home();
            return self.toast(&explain(&format!("Could not save the move: {e}")));
        }
        homedirs::run(&self.e, home, mount, job);
    }

    pub(super) fn undo_home(self: &Rc<Self>, home: &Path, mount: &Path) {
        let (ui, home, mount) = (self.clone(), home.to_path_buf(), mount.to_path_buf());
        let jobs = homedirs::jobs_root();
        glib::spawn_future_local(async move {
            let h = home.clone();
            let res = gtk::gio::spawn_blocking(move || homedirs::undo(&h, &mount, &jobs).map_err(|e| e.to_string()))
                .await
                .unwrap_or_else(|_| Err("undo crashed".to_owned()));
            ui.rebuild_home();
            ui.toast(&match res {
                Ok(()) => format!("{} is back as it was", home.display()),
                Err(e) => explain(&format!("Could not undo: {e}")),
            });
        });
    }

    pub(super) fn link_home(self: &Rc<Self>, home: &Path, mount: &Path) {
        let name = home.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let cloud = homedirs::cloud_path(mount, home);
        let d = adw::AlertDialog::new(
            Some(&format!("Move {name} to the Cloud?")),
            Some(&format!(
                "{} will become a link to {}. Your files are copied into the cloud folder; names that already exist there are kept side by side, never overwritten.\n\nYour current folder is kept as a backup in {}.\n\nWhat stays on this device follows the Folders & Cache page. Only folders set to Offline are there when Nuvolo is not running.",
                home.display(), cloud.display(), homedirs::backup_root().display()
            )),
        );
        d.add_response("cancel", "Cancel");
        d.add_response("move", "Move to Cloud");
        d.set_response_appearance("move", adw::ResponseAppearance::Suggested);
        let ui = self.clone();
        d.connect_response(Some("cancel"), move |_, _| ui.rebuild_home());
        let (ui, home, mount) = (self.clone(), home.to_path_buf(), mount.to_path_buf());
        d.connect_response(Some("move"), move |_, _| {
            if ui.e.borrow().mounted_account(&mount).is_none() {
                ui.rebuild_home();
                return ui.toast("The drive is not mounted; see Overview for why, and try again once it is.");
            };
            if let Err(e) = homedirs::link(&home, &mount, &homedirs::backup_root(), &homedirs::jobs_root()) {
                ui.rebuild_home();
                return ui.toast(&explain(&format!("Could not move {}: {e}", home.display())));
            }
            match homedirs::read_job(&homedirs::jobs_root(), &home) {
                Some(job) => homedirs::run(&ui.e, home.clone(), mount.clone(), job),
                None => ui.rebuild_home(),
            }
        });
        d.present(Some(&self.window));
    }

    pub(super) fn unlink_home(self: &Rc<Self>, home: &Path, mount: &Path) {
        let name = home.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let d = adw::AlertDialog::new(
            Some(&format!("Move {name} Back to This Device?")),
            Some(&format!("Nuvolo downloads a copy of the cloud folder into {}. The cloud copy is kept.", home.display())),
        );
        d.add_response("cancel", "Cancel");
        d.add_response("back", "Move Back");
        let ui = self.clone();
        d.connect_response(Some("cancel"), move |_, _| ui.rebuild_home());
        let (ui, home, mount) = (self.clone(), home.to_path_buf(), mount.to_path_buf());
        d.connect_response(Some("back"), move |_, _| {
            if ui.e.borrow().mounted_account(&mount).is_none() {
                ui.rebuild_home();
                return ui.toast("The drive is not mounted; see Overview for why, and try again once it is.");
            }
            if let Err(e) = homedirs::start_unlink(&home, &mount, &homedirs::jobs_root()) {
                ui.rebuild_home();
                return ui.toast(&explain(&format!("Could not move it back: {e}")));
            }
            ui.start_home(home.clone(), mount.clone(), homedirs::Job { op: Op::Unlink, backup: None, mount: Some(mount.clone()), error: None });
        });
        d.present(Some(&self.window));
    }
}

