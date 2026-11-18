mod config;
mod dbus;
mod engine;
mod homedirs;
mod rclone;
mod tray;
mod ui;

use adw::prelude::*;
use gtk::{gio, glib};

pub const APP_ID: &str = "io.github.cyythanapong_netizen.Nuvolo";
pub const REPO_URL: &str = "https://github.com/cyythanapong-netizen/nuvolo-sync";
pub const DOCS_URL: &str = "https://cyythanapong-netizen.github.io/nuvolo-sync/";

const CSS: &str = "
.pill-badge { border-radius: 9999px; padding: 2px 10px; font-weight: bold; font-size: 0.85em; }
.pill-badge.success { background: alpha(@success_color, 0.15); color: @success_color; }
.pill-badge.warning { background: alpha(@warning_color, 0.15); color: @warning_color; }
.pill-badge.error { background: alpha(@error_color, 0.15); color: @error_color; }
.stat { padding: 14px 16px; }
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

    let e = match engine::Engine::start() {
        Ok(e) => e,
        Err(err) => {
            eprintln!("nuvolo: {err}");
            app.connect_activate(move |app| ui::fatal(app, &err));
            return;
        }
    };

    // Mounts must outlive the window: keep running until Quit.
    std::mem::forget(app.hold());

    let conn = app.dbus_connection();
    if let Some(conn) = &conn {
        dbus::export(conn, e.clone());
    }

    let quit = gio::ActionEntry::builder("quit").activate(|app: &adw::Application, _, _| app.quit()).build();
    let e2 = e.clone();
    let add = gio::ActionEntry::builder("add-account")
        .activate(move |app: &adw::Application, _, _| ui::present(app, &e2).add_account())
        .build();
    let e6 = e.clone();
    let about = gio::ActionEntry::builder("about")
        .activate(move |app: &adw::Application, _, _| ui::present(app, &e6).about())
        .build();
    app.add_action_entries([quit, add, about]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);

    let (tray, tray_rx) = tray::spawn();
    let (app2, e5) = (app.clone(), e.clone());
    glib::spawn_future_local(async move {
        while let Ok(cmd) = tray_rx.recv().await {
            match cmd {
                tray::Cmd::Open => {
                    ui::present(&app2, &e5).show_page("overview");
                }
                tray::Cmd::Preferences => ui::present(&app2, &e5).show_page("prefs"),
                tray::Cmd::TogglePause => {
                    let on = !e5.borrow().paused;
                    if let Err(err) = e5.borrow_mut().set_paused(on) {
                        eprintln!("nuvolo: pause: {err}");
                    }
                }
                tray::Cmd::Quit => app2.quit(),
            }
        }
    });

    let e3 = e.clone();
    let mut last_status = String::new();
    glib::timeout_add_seconds_local(1, move || {
        let changed = e3.borrow_mut().tick();
        if let Some(conn) = &conn {
            dbus::emit_changed(conn, &changed);
        }
        ui::refresh();
        if let Some(tray) = &tray {
            let (status, paused) = tray_status(&e3.borrow());
            if status != last_status {
                last_status = status.clone();
                tray.update(move |t| {
                    t.status = status;
                    t.paused = paused;
                });
            }
        }
        glib::ControlFlow::Continue
    });

    let e4 = e.clone();
    app.connect_activate(move |app| {
        ui::present(app, &e4);
    });
    app.connect_shutdown(move |_| e.borrow_mut().shutdown());
}

/// One-line status for the tray menu and tooltip.
fn tray_status(e: &engine::Engine) -> (String, bool) {
    let active = e.core["transferring"].as_array().map_or(0, Vec::len);
    let status = if e.paused {
        "Paused".to_owned()
    } else if !e.mount_errors.is_empty() {
        format!("{} account(s) not mounted", e.mount_errors.len())
    } else if active > 0 {
        format!("Syncing {active} file(s)")
    } else {
        "Up to date".to_owned()
    };
    (status, e.paused)
}
