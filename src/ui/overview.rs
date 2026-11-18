//! The Overview page: mount status with a Retry button, offline copies, sync switches and
//! live statistics.

use super::widgets::*;
use super::Ui;
use crate::config::Mode;
use crate::engine::MountState;
use crate::errors::{explain, is_offline};
use adw::prelude::*;
use gtk::glib;
use std::rc::Rc;

pub(super) struct Overview {
    pub mount_row: adw::ActionRow,
    mount_pill: gtk::Label,
    retry_btn: gtk::Button,
    offline_row: adw::ActionRow,
    pub ondemand: adw::SwitchRow,
    pub bgsync: adw::SwitchRow,
    pause_row: adw::ActionRow,
    pause_btn: gtk::Button,
    grid: gtk::Grid,
    stat_fps: gtk::Label,
    stat_speed: gtk::Label,
    stat_speed_sub: gtk::Label,
    stat_streams: gtk::Label,
    stat_cache: gtk::Label,
    stat_cache_sub: gtk::Label,
    cache_bar: gtk::ProgressBar,
    stat_errors: gtk::Label,
    stat_errors_sub: gtk::Label,
    recent: gtk::ListBox,
}

impl Overview {
    pub fn new() -> Overview {
        let mount_row = adw::ActionRow::builder().title("Mount Location").subtitle_lines(0).build();
        mount_row.add_prefix(&gtk::Image::from_icon_name("folder-remote-symbolic"));
        let mount_pill = pill("Starting", "warning");
        mount_row.add_suffix(&mount_pill);
        let retry_btn = button("Retry");
        retry_btn.set_tooltip_text(Some("Mount the drive again now"));
        retry_btn.set_visible(false);
        mount_row.add_suffix(&retry_btn);
        let offline_row = adw::ActionRow::builder().title("Offline Copies").subtitle_lines(0).build();
        offline_row.add_prefix(&gtk::Image::from_icon_name("folder-download-symbolic"));
        let ondemand = adw::SwitchRow::builder()
            .title("On-Demand Streaming")
            .subtitle("Files download only when opened. Turn off to keep the whole drive offline.")
            .build();
        let bgsync = adw::SwitchRow::builder().title("Background Sync").subtitle("Refresh pinned folders from the cloud every hour").build();
        let pause_row = adw::ActionRow::builder().title("Transfers").build();
        let pause_btn = button("Pause All");
        pause_btn.add_css_class("pill");
        pause_row.add_suffix(&pause_btn);

        let (c_fps, stat_fps, _) = stat_card("Files / sec");
        let (c_speed, stat_speed, stat_speed_sub) = stat_card("Data Speed");
        let (c_streams, stat_streams, _) = stat_card("Active Streams");
        let (c_err, stat_errors, stat_errors_sub) = stat_card("Errors");
        let (c_cache, stat_cache, stat_cache_sub) = stat_card("VFS Cache Used");
        let cache_bar = gtk::ProgressBar::new();
        c_cache.insert_child_after(&cache_bar, Some(&stat_cache));
        let grid = gtk::Grid::builder().column_spacing(12).row_spacing(12).column_homogeneous(true).build();
        grid.attach(&c_fps, 0, 0, 1, 1);
        grid.attach(&c_speed, 1, 0, 1, 1);
        grid.attach(&c_streams, 2, 0, 1, 1);
        grid.attach(&c_err, 0, 1, 1, 1);
        grid.attach(&c_cache, 1, 1, 2, 1);
        Overview {
            mount_row,
            mount_pill,
            retry_btn,
            offline_row,
            ondemand,
            bgsync,
            pause_row,
            pause_btn,
            grid,
            stat_fps,
            stat_speed,
            stat_speed_sub,
            stat_streams,
            stat_cache,
            stat_cache_sub,
            cache_bar,
            stat_errors,
            stat_errors_sub,
            recent: boxed(),
        }
    }
}

impl Ui {
    pub(super) fn overview_page(self: &Rc<Self>) -> adw::PreferencesPage {
        let ov = &self.ov;
        let page = adw::PreferencesPage::new();
        let g = group("Mount & Sync Mode", None);
        let open = button("Open in Files");
        let ui = self.clone();
        open.connect_clicked(move |_| ui.open_mount());
        ov.mount_row.add_suffix(&open);
        let ui = self.clone();
        ov.retry_btn.connect_clicked(move |_| {
            ui.e.borrow_mut().mount(ui.acct.get());
            ui.tick_mount();
        });
        g.add(&ov.mount_row);
        g.add(&ov.offline_row);
        g.add(&ov.ondemand);
        g.add(&ov.bgsync);
        g.add(&ov.pause_row);
        page.add(&g);
        let g = group("Live Throughput & Cache", None);
        g.add(&ov.grid);
        page.add(&g);
        let g = group("Recent Activity", None);
        g.add(&ov.recent);
        page.add(&g);

        let ui = self.clone();
        ov.ondemand.connect_active_notify(move |r| {
            if ui.updating.get() {
                return;
            }
            let mode = if r.is_active() { Mode::OnDemand } else { Mode::Pinned };
            let res = ui.e.borrow_mut().set_mode(ui.acct.get(), "", mode);
            ui.toast(&match res {
                Ok(()) if mode == Mode::Pinned => "Downloading the whole drive to keep it offline".to_owned(),
                Ok(()) => "On-Demand streaming on".to_owned(),
                Err(e) => explain(&e),
            });
        });
        let ui = self.clone();
        ov.bgsync.connect_active_notify(move |r| {
            if ui.updating.get() {
                return;
            }
            let mut e = ui.e.borrow_mut();
            if let Some(a) = e.cfg.accounts.get_mut(ui.acct.get()) {
                a.background_sync = r.is_active();
            }
            e.save();
        });
        let ui = self.clone();
        ov.pause_btn.connect_clicked(move |_| {
            let on = !ui.e.borrow().paused;
            if let Err(e) = ui.e.borrow_mut().set_paused(on) {
                ui.toast(&explain(&e));
            }
            ui.tick();
        });
        page
    }

    /// The mount and offline-copy rows, shown on every tick whatever the page.
    pub(super) fn tick_mount(&self) {
        let (ov, e, i) = (&self.ov, self.e.borrow(), self.acct.get());
        let Some(a) = e.cfg.accounts.get(i) else { return };
        let path = glib::markup_escape_text(&a.mount.to_string_lossy()).to_string();
        let state = e.mount_state(i);
        ov.retry_btn.set_visible(matches!(state, MountState::Failed(_)));
        match &state {
            MountState::Mounting => {
                set_pill(&ov.mount_pill, "Mounting…", "warning");
                ov.mount_row.set_subtitle(&path);
                ov.mount_row.set_tooltip_text(None);
            }
            MountState::Failed(err) => {
                set_pill(&ov.mount_pill, if is_offline(err) { "Offline" } else { "Not Mounted" }, "error");
                ov.mount_row.set_subtitle(&glib::markup_escape_text(&explain(err)));
                ov.mount_row.set_tooltip_text(Some(err)); // the raw error, for bug reports
            }
            MountState::Mounted => {
                ov.mount_row.set_subtitle(&path);
                ov.mount_row.set_tooltip_text(None);
                if e.paused {
                    set_pill(&ov.mount_pill, "Mounted · Paused", "warning");
                } else {
                    set_pill(&ov.mount_pill, if a.on_demand() { "Mounted · VFS Full" } else { "Mounted · Offline" }, "success");
                }
            }
        }
        let pinned = a.modes.values().filter(|m| **m == Mode::Pinned).count();
        ov.offline_row.set_visible(pinned > 0);
        let n = format!("{pinned} folder{} kept offline", if pinned == 1 { "" } else { "s" });
        ov.offline_row.set_subtitle(&glib::markup_escape_text(&if state != MountState::Mounted {
            format!("{n}. While the drive is not mounted, {} shows these offline copies.", a.mount.display())
        } else if e.offline_syncing(i) {
            format!("{n} · syncing with the cloud…")
        } else if let Some(err) = e.offline_error(&a.remote) {
            format!("{n} · not up to date: {}", explain(err))
        } else {
            format!("{n} · up to date, and available when Nuvolo is not running")
        }));
    }

    pub(super) fn tick_overview(&self, fresh: bool) {
        let (ov, e) = (&self.ov, self.e.borrow());
        let Some(a) = e.cfg.accounts.get(self.acct.get()) else { return };
        let transferring = e.core["transferring"].as_array().map_or(0, Vec::len);
        ov.pause_btn.set_label(if e.paused { "Resume All" } else { "Pause All" });
        ov.pause_row.set_subtitle(&format!("{transferring} active · {} uploads waiting", e.pending_uploads(&a.remote)));
        ov.stat_fps.set_label(&e.files_per_sec.to_string());
        ov.stat_speed.set_label(&format!("{}/s", human(e.speed)));
        ov.stat_speed_sub.set_label(&format!("{} transferred this session", human(e.core["bytes"].as_f64().unwrap_or(0.0))));
        ov.stat_streams.set_label(&transferring.to_string());
        let used = e.vfs.get(&a.remote).and_then(|v| v["diskCache"]["bytesUsed"].as_f64()).unwrap_or(0.0);
        let max = e.cfg.cache_max_size_gib as f64 * 1024f64.powi(3);
        ov.stat_cache.set_label(&format!("{} / {} GiB", human(used), e.cfg.cache_max_size_gib));
        ov.cache_bar.set_fraction((used / max).min(1.0));
        let files = e.vfs.get(&a.remote).and_then(|v| v["diskCache"]["files"].as_u64()).unwrap_or(0);
        ov.stat_cache_sub.set_label(&format!("{files} files · max age {} h", e.cfg.cache_max_age_hours));
        ov.stat_errors.set_label(&e.core["errors"].as_u64().unwrap_or(0).to_string());
        let last = e.core["lastError"].as_str().map_or_else(|| "No errors".to_owned(), explain);
        ov.stat_errors_sub.set_label(&last);
        ov.stat_errors_sub.set_tooltip_text(Some(&last));
        if fresh {
            ov.recent.remove_all();
            for t in e.transferred.iter().rev().take(6) {
                ov.recent.append(&done_row(t));
            }
            if e.transferred.is_empty() {
                ov.recent.append(&placeholder("No transfers yet"));
            }
        }
    }
}
