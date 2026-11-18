mod config;
mod dbus;
mod engine;
mod rclone;
mod ui;

use adw::prelude::*;
use gtk::{gio, glib};

pub const APP_ID: &str = "io.github.cyythanapong_netizen.Nuvolo";

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
    app.add_action_entries([quit, add]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);

    let e3 = e.clone();
    glib::timeout_add_seconds_local(1, move || {
        let changed = e3.borrow_mut().tick();
        if let Some(conn) = &conn {
            dbus::emit_changed(conn, &changed);
        }
        ui::refresh();
        glib::ControlFlow::Continue
    });

    let e4 = e.clone();
    app.connect_activate(move |app| {
        ui::present(app, &e4);
    });
    app.connect_shutdown(move |_| e.borrow_mut().shutdown());
}
