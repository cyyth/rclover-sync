//! rclover: cloud drives on demand for GNOME, powered by rclone.
//!
//! This file only wires the parts together. Where to look:
//!
//! - [`engine`]: the sync policy and all state (mounts, offline copies, pins, file states).
//! - [`rclone`]: everything that talks to rclone (daemon, API client, mount, bisync, links).
//! - [`homedirs`]: moving Documents, Music, ... into the cloud and back.
//! - [`ui`]: the window, one file per page.
//! - [`dbus`]: the interface for the GNOME Files extension (contract: docs/dbus.rst).
//! - [`tray`]: the top-bar icon.
//! - [`notify`]: desktop notifications, in bulk.
//! - [`errors`]: what an error means and what to do about it.
//! - [`config`]: the settings file.
//! - [`migrate`]: the one-time move from the old name, Nuvolo.

mod config;
mod dbus;
mod engine;
mod errors;
mod homedirs;
mod migrate;
mod notify;
mod rclone;
mod tray;
mod ui;

use adw::prelude::*;
use engine::Shared;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const APP_ID: &str = "io.github.cyythanapong_netizen.Rclover";
pub const REPO_URL: &str = "https://github.com/cyythanapong-netizen/rclover-sync";
pub const DOCS_URL: &str = "https://cyythanapong-netizen.github.io/rclover-sync/";

const CSS: &str = "
.pill-badge { border-radius: 9999px; padding: 2px 10px; font-weight: bold; font-size: 0.85em; }
.pill-badge.success { background: alpha(@success_color, 0.15); color: @success_color; }
.pill-badge.warning { background: alpha(@warning_color, 0.15); color: @warning_color; }
.pill-badge.error { background: alpha(@error_color, 0.15); color: @error_color; }
.stat { padding: 14px 16px; }
progressbar.storage-pinned > trough > progress { background-color: @blue_3; border-color: @blue_3; }
progressbar.storage-cache > trough > progress { background-color: @green_4; border-color: @green_4; }
";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(startup);
    app.run()
}

fn startup(app: &adw::Application) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }

    let e = match migrate::from_nuvolo().and_then(|()| engine::Engine::start()) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("rclover: {err}");
            let err = errors::explain(&err);
            app.connect_activate(move |app| ui::fatal(app, &err));
            return;
        }
    };
    homedirs::adopt_old_moves(&homedirs::jobs_root(), &homedirs::backup_root());
    homedirs::keep_linked_user_dirs(&e.borrow().cfg.accounts.iter().map(|a| a.mount.clone()).collect::<Vec<_>>());

    // Mounts must outlive the window: keep running until Quit.
    std::mem::forget(app.hold());

    let conn = app.dbus_connection();
    if let Some(conn) = &conn {
        dbus::export(conn, e.clone());
    }
    add_actions(app, &e);
    let tray = start_tray(app, &e);

    // Mount right away when the network comes back; unmount after a 15 s grace period when
    // the network drops so pinned offline copies underneath the mount become readable.
    let e2 = e.clone();
    let offline_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    gio::NetworkMonitor::default().connect_network_changed(move |_, available| {
        if let Some(id) = offline_timer.borrow_mut().take() {
            id.remove();
        }
        if available {
            e2.borrow_mut().retry_mounts();
        } else {
            let (e3, timer_ref) = (e2.clone(), offline_timer.clone());
            let id = glib::timeout_add_seconds_local_once(15, move || {
                *timer_ref.borrow_mut() = None;
                e3.borrow_mut().unmount_for_offline();
            });
            *offline_timer.borrow_mut() = Some(id);
        }
    });

    start_ticking(&e, conn, tray);

    let e4 = e.clone();
    app.connect_activate(move |app| {
        ui::present(app, &e4);
    });
    app.connect_shutdown(move |_| e.borrow_mut().shutdown());
}

fn add_actions(app: &adw::Application, e: &Shared) {
    let quit = gio::ActionEntry::builder("quit").activate(|app: &adw::Application, _, _| app.quit()).build();
    let e2 = e.clone();
    let add = gio::ActionEntry::builder("add-account")
        .activate(move |app: &adw::Application, _, _| ui::present(app, &e2).add_account())
        .build();
    let e3 = e.clone();
    let about = gio::ActionEntry::builder("about")
        .activate(move |app: &adw::Application, _, _| ui::present(app, &e3).about())
        .build();
    let e5 = e.clone();
    let help = gio::ActionEntry::builder("help")
        .activate(move |app: &adw::Application, _, _| ui::present(app, &e5).help())
        .build();
    let e4 = e.clone();
    let show = gio::ActionEntry::builder("show-page")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(move |app: &adw::Application, _, p| {
            let page = p.and_then(|p| p.get::<String>()).unwrap_or_default();
            ui::present(app, &e4).show_page(&page);
        })
        .build();
    app.add_action_entries([quit, add, about, help, show]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("app.help", &["F1"]);
}

fn start_tray(app: &adw::Application, e: &Shared) -> Option<tray::Handle> {
    let (tray, rx) = tray::spawn();
    let (app, e) = (app.clone(), e.clone());
    glib::spawn_future_local(async move {
        while let Ok(cmd) = rx.recv().await {
            match cmd {
                tray::Cmd::Open => ui::present(&app, &e).show_page("overview"),
                tray::Cmd::RetryMounts => e.borrow_mut().retry_mounts(),
                tray::Cmd::Preferences => ui::present(&app, &e).show_page("prefs"),
                tray::Cmd::TogglePause => {
                    let on = !e.borrow().paused;
                    if let Err(err) = e.borrow_mut().set_paused(on) {
                        eprintln!("rclover: pause: {err}");
                    }
                }
                tray::Cmd::Quit => app.quit(),
            }
        }
    });
    tray
}

/// Adaptive tick: polls rclone at 1 Hz only while the window is visible and work is active;
/// drops to every 4 s when idle (visible) and every 10 s when hidden. Health checks stay at 5 s.
/// A poll that is still running (rclone busy) makes the next ones wait instead of piling up.
fn start_ticking(e: &Shared, conn: Option<gio::DBusConnection>, tray: Option<tray::Handle>) {
    let e = e.clone();
    let busy = Rc::new(Cell::new(false));
    let last_status = Rc::new(RefCell::new(String::new()));
    let errors = Rc::new(RefCell::new(notify::ErrorNotice::default()));
    glib::timeout_add_seconds_local(1, move || {
        if busy.replace(true) {
            return glib::ControlFlow::Continue;
        }
        let visible = ui::is_visible();
        let (active, ticks) = {
            let eng = e.borrow();
            (eng.is_active(), eng.ticks())
        };
        let want_stats = if visible && active {
            true
        } else if visible {
            ticks % 4 == 0
        } else {
            ticks % 10 == 0
        };
        let job = e.borrow_mut().poll_job(want_stats);
        let (e, busy, conn, tray, last_status, errors) = (e.clone(), busy.clone(), conn.clone(), tray.clone(), last_status.clone(), errors.clone());
        glib::spawn_future_local(async move {
            let Ok(poll) = gio::spawn_blocking(job).await else {
                busy.set(false);
                return;
            };
            let changed = e.borrow_mut().apply(poll);
            busy.set(false);
            if let Some(conn) = &conn {
                dbus::emit_changed(conn, &changed);
            }
            errors.borrow_mut().check(&e.borrow());
            if e.borrow().ticks() % 10 == 1 {
                homedirs::resume_all(&e);
            }
            ui::refresh();
            if let Some(tray) = &tray {
                let (status, paused, failed) = tray::status(&e.borrow());
                if *last_status.borrow() != status {
                    *last_status.borrow_mut() = status.clone();
                    tray.update(move |t| {
                        t.status = status;
                        t.paused = paused;
                        t.failed = failed;
                    });
                }
            }
        });
        glib::ControlFlow::Continue
    });
}
