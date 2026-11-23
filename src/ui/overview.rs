//! The Overview page: mount status with a Retry button, offline copies, sync switches and
//! live statistics.

use super::widgets::*;
use super::Ui;
use crate::config::Mode;
use crate::engine::{MountState, OfflineProblem};
use crate::errors::{explain, is_offline};
use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(PartialEq, Eq)]
struct MountSnapshot {
    acct: usize,
    mount_path: String,
    state: MountState,
    paused: bool,
    on_demand: bool,
    pinned: usize,
    syncing: bool,
    problem: Option<OfflineProblem>,
}

pub(super) struct Overview {
    pub mount_row: adw::ActionRow,
    mount_pill: gtk::Label,
    retry_btn: gtk::Button,
    offline_row: adw::ActionRow,
    offline_sync_btn: gtk::Button,
    offline_force_btn: gtk::Button,
    pub ondemand: adw::SwitchRow,
    pub bgsync: adw::SwitchRow,
    pause_row: adw::ActionRow,
    pause_btn: gtk::Button,
    grid: gtk::Grid,
    stat_fps: gtk::Label,
    stat_speed: gtk::Label,
    stat_speed_sub: gtk::Label,
    stat_streams: gtk::Label,
    storage: Storage,
    stat_errors: gtk::Label,
    stat_errors_sub: gtk::Label,
    recent: gtk::ListBox,
    last_mount: RefCell<Option<MountSnapshot>>,
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
        let offline_force_btn = button("Sync Anyway");
        offline_force_btn.add_css_class("destructive-action");
        offline_force_btn.set_tooltip_text(Some("Sync past the safety stop, passing the deletions or changes on"));
        offline_row.add_suffix(&offline_force_btn);
        let offline_sync_btn = button("Sync Now");
        offline_row.add_suffix(&offline_sync_btn);
        let ondemand = adw::SwitchRow::builder()
            .title("On-Demand Streaming")
            .subtitle("Files download only when opened. Turn off to keep the whole drive offline.")
            .build();
        let bgsync = adw::SwitchRow::builder().title("Background Sync").subtitle("Sync Offline folders with the cloud every 10 minutes").build();
        let pause_row = adw::ActionRow::builder().title("Transfers").build();
        let pause_btn = button("Pause All");
        pause_btn.add_css_class("pill");
        pause_row.add_suffix(&pause_btn);

        let (c_fps, stat_fps, _) = stat_card("Files / sec");
        let (c_speed, stat_speed, stat_speed_sub) = stat_card("Data Speed");
        let (c_streams, stat_streams, _) = stat_card("Active Streams");
        let (c_err, stat_errors, stat_errors_sub) = stat_card("Errors");
        stat_errors_sub.set_wrap(true);
        stat_errors_sub.set_lines(3);
        let storage = Storage::new();
        let grid = gtk::Grid::builder().column_spacing(12).row_spacing(12).column_homogeneous(true).build();
        grid.attach(&c_fps, 0, 0, 1, 1);
        grid.attach(&c_speed, 1, 0, 1, 1);
        grid.attach(&c_streams, 2, 0, 1, 1);
        grid.attach(&c_err, 0, 1, 1, 1);
        grid.attach(&storage.card, 1, 1, 2, 1);
        Overview {
            mount_row,
            mount_pill,
            retry_btn,
            offline_row,
            offline_sync_btn,
            offline_force_btn,
            ondemand,
            bgsync,
            pause_row,
            pause_btn,
            grid,
            stat_fps,
            stat_speed,
            stat_speed_sub,
            stat_streams,
            storage,
            stat_errors,
            stat_errors_sub,
            recent: boxed(),
            last_mount: RefCell::new(None),
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
        let ui = self.clone();
        ov.offline_sync_btn.connect_clicked(move |_| {
            ui.e.borrow_mut().sync_offline(ui.acct.get());
            ui.tick_mount();
        });
        let ui = self.clone();
        ov.offline_force_btn.connect_clicked(move |_| {
            ui.e.borrow_mut().sync_offline_anyway(ui.acct.get());
            ui.tick_mount();
        });
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
        let state = e.mount_state(i);
        let pinned = a.modes.values().filter(|m| **m == Mode::Pinned).count();
        let syncing = e.offline_syncing(i);
        let problem = e.offline_error(&a.remote).cloned();
        let snap = MountSnapshot {
            acct: i,
            mount_path: a.mount.to_string_lossy().into_owned(),
            state: state.clone(),
            paused: e.paused,
            on_demand: a.on_demand(),
            pinned,
            syncing,
            problem: problem.clone(),
        };
        if ov.last_mount.borrow().as_ref() == Some(&snap) {
            return;
        }
        let path = glib::markup_escape_text(&snap.mount_path).to_string();
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
        ov.offline_row.set_visible(pinned > 0);
        let n = format!("{pinned} item{} kept offline", if pinned == 1 { "" } else { "s" });
        let mounted = state == MountState::Mounted;
        ov.offline_sync_btn.set_visible(mounted && !syncing);
        ov.offline_force_btn.set_visible(mounted && !syncing && problem.as_ref().is_some_and(|p| p.can_force));
        ov.offline_row.set_subtitle(&glib::markup_escape_text(&if !mounted {
            format!("{n}. While the drive is not mounted, {} shows these offline copies.", a.mount.display())
        } else if syncing {
            format!("{n} · syncing with the cloud…")
        } else if let Some(p) = &problem {
            format!("{n} · not up to date.\n{}", p.text)
        } else {
            format!("{n} · up to date, and available when rclover is not running")
        }));
        *ov.last_mount.borrow_mut() = Some(snap);
    }

    pub(super) fn tick_overview(&self, fresh: bool) {
        let (ov, e) = (&self.ov, self.e.borrow());
        let Some(a) = e.cfg.accounts.get(self.acct.get()) else { return };
        let transferring = e.core["transferring"].as_array().map_or(0, Vec::len);
        ov.pause_btn.set_label(if e.paused { "Resume All" } else { "Pause All" });
        ov.pause_row.set_subtitle(&format!("{transferring} active · {} uploads waiting", e.pending_uploads(&a.remote)));
        ov.stat_fps.set_label(&e.files_per_sec.to_string());
        ov.stat_speed.set_label(&format!("{}/s", human(e.speed)));
        ov.stat_speed_sub.set_label(&format!("{} this session", human(e.core["bytes"].as_f64().unwrap_or(0.0))));
        ov.stat_streams.set_label(&transferring.to_string());
        let used = e.vfs.get(&a.remote).and_then(|v| v["diskCache"]["bytesUsed"].as_f64()).unwrap_or(0.0);
        let files = e.vfs.get(&a.remote).and_then(|v| v["diskCache"]["files"].as_u64()).unwrap_or(0);
        ov.storage.show(e.offline_bytes(&a.remote), used, files, e.cfg.cache_max_size_gib, e.cfg.cache_max_age_hours);
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

/// The "Storage on This Device" card: two labelled bars. Offline folders are the fixed part
/// (always kept, as large as what is pinned); the on-demand cache is the part that comes and
/// goes, up to its limit.
struct Storage {
    card: gtk::Box,
    total: gtk::Label,
    pinned_value: gtk::Label,
    pinned_bar: gtk::ProgressBar,
    cache_value: gtk::Label,
    cache_bar: gtk::ProgressBar,
    cache_note: gtk::Label,
}

impl Storage {
    fn new() -> Storage {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("card");
        card.add_css_class("stat");
        card.append(&label("Storage on This Device", &["caption-heading", "dim-label"]));
        let total = label("–", &["title-2", "numeric"]);
        card.append(&total);
        let part = |name: &str, class: &str, note: &str| {
            let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            head.set_margin_top(6);
            head.append(&label(name, &["heading"]));
            let value = label("–", &["numeric"]);
            value.set_hexpand(true);
            value.set_xalign(1.0);
            head.append(&value);
            let bar = gtk::ProgressBar::new();
            bar.add_css_class(class);
            let note = label(note, &["caption", "dim-label"]);
            note.set_ellipsize(gtk::pango::EllipsizeMode::None);
            note.set_wrap(true);
            card.append(&head);
            card.append(&bar);
            card.append(&note);
            (value, bar, note)
        };
        let (pinned_value, pinned_bar, _) = part("Offline folders", "storage-pinned", "Always on this device, also when rclover is not running. As large as what you set to Offline.");
        let (cache_value, cache_bar, cache_note) = part("On-demand cache", "storage-cache", "");
        Storage { card, total, pinned_value, pinned_bar, cache_value, cache_bar, cache_note }
    }

    fn show(&self, pinned: Option<u64>, used: f64, files: u64, limit_gib: u32, age_hours: u32) {
        let limit = limit_gib as f64 * 1024f64.powi(3);
        let p = pinned.map(|b| b as f64);
        let p_bytes = p.unwrap_or(0.0);
        let max_total = (p_bytes + limit).max(1.0);
        self.total.set_label(&format!("{} used · at most {}", human(p_bytes + used), human(p_bytes + limit)));
        self.pinned_value.set_label(&p.map_or_else(|| "measured after the next sync".to_owned(), human));
        // Scale against the combined local storage ceiling so both bars compare proportionally.
        self.pinned_bar.set_fraction((p_bytes / max_total).min(1.0));
        self.cache_value.set_label(&format!("{} of {limit_gib} GiB", human(used)));
        self.cache_bar.set_fraction((used / limit.max(1.0)).min(1.0));
        self.cache_note.set_label(&format!(
            "Files you opened from On-Demand folders ({files} now). The oldest are cleared when it is full, or after {}. Change the size in Preferences.",
            if age_hours % 24 == 0 { format!("{} days", age_hours / 24) } else { format!("{age_hours} hours") }
        ));
    }
}
