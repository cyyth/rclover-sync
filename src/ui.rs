//! The Nuvolo window: account sidebar plus Overview, Folders, Transfers and Preferences pages.

use crate::config::Mode;
use crate::homedirs;
use crate::engine::Shared;
use adw::prelude::*;
use gtk::{gio, glib, pango};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::rc::Rc;

const PAGES: [(&str, &str, &str); 4] = [
    ("overview", "Overview & Activity", "go-home-symbolic"),
    ("folders", "Folders & Cache", "folder-symbolic"),
    ("transfers", "Transfers & Queue", "network-transmit-receive-symbolic"),
    ("prefs", "Preferences", "preferences-system-symbolic"),
];
const AGES: [(u32, &str); 4] = [(1, "1 hour"), (24, "1 day"), (168, "7 days"), (720, "30 days")];

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

/// Shows the main window, building it on first use.
pub fn present(app: &adw::Application, e: &Shared) -> Rc<Ui> {
    let ui = UI.with(|u| u.borrow().clone()).unwrap_or_else(|| {
        let ui = Ui::new(app, e);
        UI.with(|u| *u.borrow_mut() = Some(ui.clone()));
        ui
    });
    ui.window.present();
    ui
}

impl Ui {
    /// Switches the window to one of the sidebar pages ("overview", "prefs", ...).
    pub fn show_page(&self, id: &str) {
        if let Some(i) = PAGES.iter().position(|p| p.0 == id) {
            self.nav.select_row(self.nav.row_at_index(i as i32).as_ref());
        }
    }
}

/// Called every second by the engine tick.
pub fn refresh() {
    if let Some(ui) = UI.with(|u| u.borrow().clone()) {
        if ui.window.is_visible() {
            ui.tick();
        }
    }
}

/// Shown instead of the main window when rclone could not be started.
pub fn fatal(app: &adw::Application, msg: &str) {
    let page = adw::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title("Nuvolo Could Not Start")
        .description(glib::markup_escape_text(msg).as_str())
        .build();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&page));
    adw::ApplicationWindow::builder().application(app).title("Nuvolo").default_width(520).default_height(420).content(&tv).build().present();
}

pub struct Ui {
    e: Shared,
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    split: adw::NavigationSplitView,
    title: adw::WindowTitle,
    banner: adw::Banner,
    stack: adw::ViewStack,
    accounts: gtk::ListBox,
    nav: gtk::ListBox,
    acct: Cell<usize>,
    updating: Cell<bool>,
    // Overview
    mount_row: adw::ActionRow,
    mount_pill: gtk::Label,
    ondemand: adw::SwitchRow,
    bgsync: adw::SwitchRow,
    pause_row: adw::ActionRow,
    pause_btn: gtk::Button,
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
    // Folders
    folders: gtk::ListBox,
    // Transfers
    active: gtk::ListBox,
    done: gtk::ListBox,
    // Preferences
    client_row: adw::ActionRow,
    ignore: gtk::ListBox,
    home: gtk::ListBox,
    /// Running home-folder jobs: folder -> (what it is doing, files copied so far).
    jobs: RefCell<HashMap<std::path::PathBuf, (&'static str, Arc<AtomicUsize>)>>,
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

fn boxed() -> gtk::ListBox {
    let l = gtk::ListBox::new();
    l.add_css_class("boxed-list");
    l.set_selection_mode(gtk::SelectionMode::None);
    l
}

fn group(title: &str, desc: Option<&str>) -> adw::PreferencesGroup {
    // Group titles and descriptions are markup: escape "&" and friends.
    let g = adw::PreferencesGroup::builder().title(glib::markup_escape_text(title).as_str()).build();
    g.set_description(desc.map(|d| glib::markup_escape_text(d)).as_deref());
    g
}

fn set_pill(l: &gtk::Label, text: &str, kind: &str) {
    l.set_label(text);
    for k in ["success", "warning", "error"] {
        l.remove_css_class(k);
    }
    l.add_css_class(kind);
}

fn pill(text: &str, kind: &str) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.add_css_class("pill-badge");
    l.set_valign(gtk::Align::Center);
    set_pill(&l, text, kind);
    l
}

fn placeholder(text: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder().title(text).build();
    r.add_css_class("dim-label");
    r
}

fn stat_card(title: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("card");
    b.add_css_class("stat");
    let v = label("–", &["title-1", "numeric"]);
    let s = label(" ", &["caption", "dim-label"]);
    b.append(&label(title, &["caption-heading", "dim-label"]));
    b.append(&v);
    b.append(&s);
    (b, v, s)
}

fn human(b: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let (mut b, mut i) = (b, 0);
    while b >= 1024.0 && i < units.len() - 1 {
        b /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b:.0} B") } else { format!("{b:.1} {}", units[i]) }
}

fn mode_label(m: Mode) -> &'static str {
    match m {
        Mode::OnDemand => "On-Demand · cloud only until opened",
        Mode::Pinned => "Always Offline · kept in the local cache",
        Mode::Excluded => "Excluded · hidden and never synced",
    }
}

fn button(text: &str) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.set_valign(gtk::Align::Center);
    b
}

impl Ui {
    fn new(app: &adw::Application, e: &Shared) -> Rc<Ui> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Nuvolo")
            .default_width(1100)
            .default_height(760)
            .hide_on_close(true)
            .build();

        // Content pane
        let title = adw::WindowTitle::new(PAGES[0].1, "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let menu = gio::Menu::new();
        menu.append(Some("Add Cloud Account…"), Some("app.add-account"));
        menu.append(Some("About Nuvolo"), Some("app.about"));
        menu.append(Some("Quit"), Some("app.quit"));
        header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).tooltip_text("Main Menu").build());
        let theme_btn = gtk::Button::builder().icon_name("weather-clear-night-symbolic").tooltip_text("Toggle light/dark style").build();
        theme_btn.add_css_class("flat");
        theme_btn.connect_clicked(|_| {
            let sm = adw::StyleManager::default();
            sm.set_color_scheme(if sm.is_dark() { adw::ColorScheme::ForceLight } else { adw::ColorScheme::ForceDark });
        });
        header.pack_end(&theme_btn);
        let banner = adw::Banner::new("This account uses rclone's shared OAuth client, so API quota is shared and transfers may be throttled.");
        banner.set_button_label(Some("Use Custom Client ID"));
        let stack = adw::ViewStack::new();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.add_top_bar(&banner);
        tv.set_content(Some(&stack));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&tv));

        // Sidebar
        let accounts = gtk::ListBox::new();
        accounts.add_css_class("navigation-sidebar");
        accounts.set_selection_mode(gtk::SelectionMode::None);
        let nav = gtk::ListBox::new();
        nav.add_css_class("navigation-sidebar");
        for (id, text, icon) in PAGES {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.append(&gtk::Image::from_icon_name(icon));
            row.append(&label(text, &[]));
            let r = gtk::ListBoxRow::builder().child(&row).name(id).build();
            nav.append(&r);
        }
        let sb = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let heading = |t: &str| {
            let l = label(t, &["heading", "dim-label"]);
            l.set_margin_start(18);
            l.set_margin_top(12);
            l.set_margin_bottom(4);
            l
        };
        sb.append(&heading("Accounts"));
        sb.append(&accounts);
        sb.append(&heading("Navigation"));
        sb.append(&nav);
        let sb_tv = adw::ToolbarView::new();
        sb_tv.add_top_bar(&adw::HeaderBar::new());
        sb_tv.set_content(Some(&gtk::ScrolledWindow::builder().child(&sb).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build()));

        let split = adw::NavigationSplitView::new();
        split.set_sidebar(Some(&adw::NavigationPage::new(&sb_tv, "Nuvolo")));
        split.set_content(Some(&adw::NavigationPage::new(&toasts, "Nuvolo")));
        window.set_content(Some(&split));
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 640sp").expect("valid breakpoint"));
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);

        // Overview widgets
        let mount_row = adw::ActionRow::builder().title("Mount Location").build();
        mount_row.add_prefix(&gtk::Image::from_icon_name("folder-remote-symbolic"));
        let mount_pill = pill("Starting", "warning");
        mount_row.add_suffix(&mount_pill);
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

        let ui = Rc::new(Ui {
            e: e.clone(),
            window,
            toasts,
            split,
            title,
            banner,
            stack,
            accounts,
            nav,
            acct: Cell::new(0),
            updating: Cell::new(false),
            mount_row,
            mount_pill,
            ondemand,
            bgsync,
            pause_row,
            pause_btn,
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
            folders: boxed(),
            active: boxed(),
            done: boxed(),
            client_row: adw::ActionRow::builder().title("OAuth Client").build(),
            ignore: boxed(),
            home: boxed(),
            jobs: RefCell::new(HashMap::new()),
        });
        ui.build_pages(&grid);
        ui.wire();
        ui.rebuild_accounts();
        ui.select_account(0);
        ui.nav.select_row(ui.nav.row_at_index(0).as_ref());
        ui
    }

    fn build_pages(self: &Rc<Self>, grid: &gtk::Grid) {
        // Overview
        let page = adw::PreferencesPage::new();
        let g = group("Mount & Sync Mode", None);
        let open = button("Open in Files");
        let ui = self.clone();
        open.connect_clicked(move |_| ui.open_mount());
        self.mount_row.add_suffix(&open);
        g.add(&self.mount_row);
        g.add(&self.ondemand);
        g.add(&self.bgsync);
        g.add(&self.pause_row);
        page.add(&g);
        let g = group("Live Throughput & Cache", None);
        g.add(grid);
        page.add(&g);
        let g = group("Recent Activity", None);
        g.add(&self.recent);
        page.add(&g);
        self.stack.add_named(&page, Some("overview"));

        // Folders
        let page = adw::PreferencesPage::new();
        let g = group("Cloud Folders", Some("Choose what stays on this device. On-Demand folders use no disk space until a file is opened."));
        let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
        reload.add_css_class("flat");
        reload.set_tooltip_text(Some("Reload folders"));
        let ui = self.clone();
        reload.connect_clicked(move |_| ui.load_folders());
        g.set_header_suffix(Some(&reload));
        g.add(&self.folders);
        page.add(&g);
        self.stack.add_named(&page, Some("folders"));

        // Transfers
        let page = adw::PreferencesPage::new();
        let g = group("Active", Some("Uploads and downloads running now."));
        g.add(&self.active);
        page.add(&g);
        let g = group("Recently Completed", None);
        g.add(&self.done);
        page.add(&g);
        self.stack.add_named(&page, Some("transfers"));

        self.stack.add_named(&self.prefs_page(), Some("prefs"));
    }

    fn prefs_page(self: &Rc<Self>) -> adw::PreferencesPage {
        let cfg = self.e.borrow().cfg.clone();
        let page = adw::PreferencesPage::new();

        let g = group("General", None);
        let login = adw::SwitchRow::builder()
            .title("Start at Login")
            .subtitle("Mount your drives in the background when you log in")
            .active(autostart_file().exists())
            .build();
        let ui = self.clone();
        login.connect_active_notify(move |r| {
            if let Err(e) = set_autostart(r.is_active()) {
                ui.toast(&format!("Could not change login startup: {e}"));
            }
        });
        g.add(&login);
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
        let size = spin("Max VFS Cache Size (GiB)", "--vfs-cache-max-size", 2000.0, cfg.cache_max_size_gib);
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
            Some("Keep these folders in the cloud. The folder in your home becomes a link to its copy in the drive, which stays available offline. Your current folder is kept as a backup."),
        );
        g.add(&self.home);
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

    fn wire(self: &Rc<Self>) {
        let ui = self.clone();
        self.nav.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let id = row.widget_name();
            if let Some((_, text, _)) = PAGES.iter().find(|p| p.0 == id.as_str()) {
                ui.stack.set_visible_child_name(&id);
                ui.title.set_title(text);
                ui.split.set_show_content(true);
                ui.tick();
            }
        });
        let ui = self.clone();
        self.accounts.connect_row_activated(move |_, row| {
            let i = row.index() as usize;
            if i < ui.e.borrow().cfg.accounts.len() {
                ui.select_account(i);
            } else {
                ui.add_account();
            }
        });
        let ui = self.clone();
        self.ondemand.connect_active_notify(move |r| {
            if ui.updating.get() {
                return;
            }
            let mode = if r.is_active() { Mode::OnDemand } else { Mode::Pinned };
            let res = ui.e.borrow_mut().set_mode(ui.acct.get(), "", mode);
            ui.toast(&match res {
                Ok(()) if mode == Mode::Pinned => "Downloading the whole drive to keep it offline".to_owned(),
                Ok(()) => "On-Demand streaming on".to_owned(),
                Err(e) => e,
            });
        });
        let ui = self.clone();
        self.bgsync.connect_active_notify(move |r| {
            if ui.updating.get() {
                return;
            }
            let mut e = ui.e.borrow_mut();
            let i = ui.acct.get();
            if let Some(a) = e.cfg.accounts.get_mut(i) {
                a.background_sync = r.is_active();
            }
            e.save();
        });
        let ui = self.clone();
        self.pause_btn.connect_clicked(move |_| {
            let on = !ui.e.borrow().paused;
            if let Err(e) = ui.e.borrow_mut().set_paused(on) {
                ui.toast(&e);
            }
            ui.tick();
        });
        let ui = self.clone();
        self.banner.connect_button_clicked(move |_| ui.client_id_dialog());
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(&glib::markup_escape_text(msg)));
    }

    fn toast_remount(self: &Rc<Self>, msg: &str) {
        let t = adw::Toast::builder().title(glib::markup_escape_text(msg).as_str()).button_label("Remount Now").build();
        let ui = self.clone();
        t.connect_button_clicked(move |_| {
            let n = ui.e.borrow().cfg.accounts.len();
            let errors: Vec<String> = (0..n).filter_map(|i| ui.e.borrow_mut().remount(i).err()).collect();
            ui.toast(&if errors.is_empty() { "Remounted".to_owned() } else { errors.join("; ") });
        });
        self.toasts.add_toast(t);
    }

    fn rebuild_accounts(self: &Rc<Self>) {
        self.accounts.remove_all();
        for a in &self.e.borrow().cfg.accounts {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.append(&adw::Avatar::new(32, Some(&a.remote), true));
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            text.set_hexpand(true);
            text.append(&label(&a.remote, &[]));
            text.append(&label(&a.mount.to_string_lossy(), &["caption", "dim-label"]));
            row.append(&text);
            self.accounts.append(&row);
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&gtk::Image::from_icon_name("list-add-symbolic"));
        row.append(&label("Add Cloud Account", &["accent"]));
        self.accounts.append(&row);
    }

    fn select_account(self: &Rc<Self>, i: usize) {
        self.acct.set(i);
        let (remote, mount, bg, on_demand) = {
            let e = self.e.borrow();
            let Some(a) = e.cfg.accounts.get(i) else {
                self.title.set_subtitle("No account yet");
                self.mount_row.set_subtitle("Add an rclone remote to get started");
                self.banner.set_revealed(false);
                return;
            };
            (a.remote.clone(), a.mount.to_string_lossy().into_owned(), a.background_sync, a.on_demand())
        };
        self.title.set_subtitle(&remote);
        self.mount_row.set_subtitle(&glib::markup_escape_text(&mount));
        self.updating.set(true);
        self.ondemand.set_active(on_demand);
        self.bgsync.set_active(bg);
        self.updating.set(false);
        let info = self.e.borrow().client.call("config/get", json!({ "name": remote })).unwrap_or(Value::Null);
        let client_id = info["client_id"].as_str().unwrap_or("");
        let shared = info["type"] == "drive" && client_id.is_empty();
        self.banner.set_revealed(shared);
        self.client_row.set_subtitle(if client_id.is_empty() { "rclone shared client (rate-limited)" } else { client_id });
        self.load_folders();
        self.rebuild_home();
        self.tick();
    }

    fn open_mount(&self) {
        let Some(mount) = self.e.borrow().cfg.accounts.get(self.acct.get()).map(|a| a.mount.clone()) else { return };
        let uri = gio::File::for_path(mount).uri();
        if let Err(err) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
            self.toast(&err.to_string());
        }
    }

    fn load_folders(self: &Rc<Self>) {
        let job = {
            let e = self.e.borrow();
            e.cfg.accounts.get(self.acct.get()).map(|a| (e.client.clone(), a.fs()))
        };
        self.folders.remove_all();
        let Some((client, fs_)) = job else { return };
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
                Err(e) => ui.folders.append(&placeholder(&format!("Could not list folders: {e}"))),
            }
        });
    }

    fn folder_row(self: &Rc<Self>, name: &str) -> adw::ActionRow {
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

    /// Rows for the XDG folders: "Move to <account>" or "Move Back to This Device".
    fn rebuild_home(self: &Rc<Self>) {
        self.home.remove_all();
        let Some(mount) = self.e.borrow().cfg.accounts.get(self.acct.get()).map(|a| a.mount.clone()) else {
            self.home.append(&placeholder("Add a cloud account first"));
            return;
        };
        for dir in homedirs::FOLDERS {
            let Some(home) = homedirs::home_path(dir) else { continue };
            let linked = homedirs::is_linked(&home, &mount);
            let name = home.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name).as_str())
                .subtitle(glib::markup_escape_text(&if linked {
                    format!("Stored in {}", homedirs::cloud_path(&mount, &home).display())
                } else {
                    format!("On this device ({})", home.display())
                }).as_str())
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(if linked { "folder-remote-symbolic" } else { "folder-symbolic" }));
            if let Some((doing, n)) = self.jobs.borrow().get(&home) {
                // Busy: a spinner and a live count instead of the button, so it cannot be started twice.
                row.set_subtitle(&format!("{doing}… {} files so far", n.load(Ordering::Relaxed)));
                let spinner = gtk::Spinner::builder().spinning(true).valign(gtk::Align::Center).build();
                row.add_suffix(&spinner);
            } else {
                let btn = button(if linked { "Move Back" } else { "Move to Cloud" });
                let (ui, home2, mount2) = (self.clone(), home.clone(), mount.clone());
                btn.connect_clicked(move |b| {
                    b.set_sensitive(false); // until the dialog is answered or the job starts
                    if linked { ui.unlink_home(&home2, &mount2) } else { ui.link_home(&home2, &mount2) }
                });
                row.add_suffix(&btn);
            }
            self.home.append(&row);
        }
    }

    /// Runs a slow home-folder job off the main thread. While it runs the folder's row shows a
    /// spinner and a file count, and starting another job for the same folder is refused.
    fn run_home_job(
        self: &Rc<Self>,
        home: std::path::PathBuf,
        doing: &'static str,
        work: impl FnOnce(&AtomicUsize) -> std::io::Result<()> + Send + 'static,
        done: impl FnOnce(&Rc<Ui>, Result<(), String>) + 'static,
    ) {
        if self.jobs.borrow().contains_key(&home) {
            return self.toast("That folder is already being moved");
        }
        let count = Arc::new(AtomicUsize::new(0));
        let first = self.jobs.borrow().is_empty();
        self.jobs.borrow_mut().insert(home.clone(), (doing, count.clone()));
        self.rebuild_home();
        if first {
            // One timer refreshes the progress of all running jobs, and stops with the last one.
            let ui = self.clone();
            glib::timeout_add_seconds_local(1, move || {
                if ui.jobs.borrow().is_empty() {
                    return glib::ControlFlow::Break;
                }
                ui.rebuild_home();
                glib::ControlFlow::Continue
            });
        }
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || work(&count).map_err(|e| e.to_string()))
                .await
                .unwrap_or_else(|_| Err("the job crashed".to_owned()));
            ui.jobs.borrow_mut().remove(&home);
            ui.rebuild_home();
            done(&ui, res);
        });
    }

    fn link_home(self: &Rc<Self>, home: &std::path::Path, mount: &std::path::Path) {
        let name = home.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let cloud = homedirs::cloud_path(mount, home);
        let d = adw::AlertDialog::new(
            Some(&format!("Move {name} to the Cloud?")),
            Some(&format!(
                "{} will become a link to {}, kept available offline. Your files are copied into the cloud folder; names that already exist there are kept side by side, never overwritten.\n\nYour current folder is kept as a backup in {}.",
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
            let i = ui.acct.get();
            if !ui.e.borrow().is_mounted(i) {
                ui.rebuild_home();
                return ui.toast("The drive is not mounted; try again once it is.");
            }
            let backup = match homedirs::link(&home, &mount, &homedirs::backup_root()) {
                Ok(b) => b,
                Err(e) => {
                    ui.rebuild_home();
                    return ui.toast(&format!("Could not move {}: {e}", home.display()));
                }
            };
            let rel = cloud.strip_prefix(&mount).unwrap_or(&cloud).to_string_lossy().into_owned();
            if let Err(e) = ui.e.borrow_mut().set_mode(i, &rel, Mode::Pinned) {
                eprintln!("nuvolo: pin {rel}: {e}");
            }
            let (b, c) = (backup.clone(), cloud.clone());
            ui.run_home_job(
                home.clone(),
                "Copying to the cloud",
                move |n| homedirs::merge_into(&b, &c, n),
                move |ui, res| {
                    ui.toast(&match res {
                        Ok(()) => format!("Done. The original is kept in {}", backup.display()),
                        Err(e) => format!("Copy stopped: {e}. Nothing was lost: the original is in {}", backup.display()),
                    })
                },
            );
        });
        d.present(Some(&self.window));
    }

    fn unlink_home(self: &Rc<Self>, home: &std::path::Path, mount: &std::path::Path) {
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
            let (h, m, shown) = (home.clone(), mount.clone(), home.clone());
            ui.run_home_job(
                home.clone(),
                "Downloading to this device",
                move |n| homedirs::unlink(&h, &m, n),
                move |ui, res| {
                    ui.toast(&match res {
                        Ok(()) => format!("{} is back on this device", shown.display()),
                        Err(e) => format!("Could not move it back: {e}. The link is unchanged."),
                    })
                },
            );
        });
        d.present(Some(&self.window));
    }

    pub fn about(&self) {
        let d = adw::AboutDialog::builder()
            .application_name("Nuvolo")
            .application_icon(crate::APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .comments("Cloud drives on demand for GNOME, powered by rclone.")
            .developer_name("Nuvolo contributors")
            .license_type(gtk::License::Gpl30)
            .website(crate::REPO_URL)
            .issue_url(format!("{}/issues", crate::REPO_URL))
            .build();
        d.add_link("Documentation", crate::DOCS_URL);
        d.add_link("GNOME Files Extension", "https://github.com/cyythanapong-netizen/nuvolo-nautilus");
        d.add_link("rclone", "https://rclone.org");
        d.present(Some(&self.window));
    }

    fn rebuild_ignore(self: &Rc<Self>) {
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

    pub fn add_account(self: &Rc<Self>) {
        let (remotes, have) = {
            let e = self.e.borrow();
            let v = e.client.call("config/listremotes", json!({})).unwrap_or(Value::Null);
            let remotes: Vec<String> = v["remotes"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_owned)).collect();
            (remotes, e.cfg.accounts.iter().map(|a| a.remote.clone()).collect::<Vec<_>>())
        };
        let avail: Vec<String> = remotes.into_iter().filter(|r| !have.contains(r)).collect();
        if avail.is_empty() {
            let d = adw::AlertDialog::new(
                Some("No Remotes to Add"),
                Some("Create a remote first by running “rclone config” in a terminal, then try again."),
            );
            d.add_response("ok", "OK");
            d.present(Some(&self.window));
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
        let (ui, dlg) = (self.clone(), d.clone());
        list.connect_row_activated(move |_, row| {
            let i = ui.e.borrow_mut().add_account(&avail[row.index() as usize]);
            dlg.close();
            ui.rebuild_accounts();
            ui.select_account(i);
        });
        d.present(Some(&self.window));
    }

    fn client_id_dialog(self: &Rc<Self>) {
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

    fn reconnect(self: &Rc<Self>, btn: &gtk::Button) {
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
                return ui.toast("Reconnect failed or was cancelled");
            }
            let res = ui.e.borrow_mut().remount(i);
            ui.toast(&res.map_or_else(|e| format!("Reconnected; remount later ({e})"), |_| "Account reconnected".to_owned()));
        });
    }

    fn tick(&self) {
        let e = self.e.borrow();
        let page = self.stack.visible_child_name().unwrap_or_default();
        let Some(a) = e.cfg.accounts.get(self.acct.get()) else { return };

        match e.mount_errors.get(&a.remote) {
            Some(err) => {
                set_pill(&self.mount_pill, "Not Mounted", "error");
                self.mount_row.set_tooltip_text(Some(err));
            }
            None if e.paused => set_pill(&self.mount_pill, "Mounted · Paused", "warning"),
            None => set_pill(&self.mount_pill, if a.on_demand() { "Mounted · VFS Full" } else { "Mounted · Offline" }, "success"),
        }
        let transferring = e.core["transferring"].as_array().map_or(0, Vec::len);
        let pending = e.pending_uploads(&a.remote);
        self.pause_btn.set_label(if e.paused { "Resume All" } else { "Pause All" });
        self.pause_row.set_subtitle(&format!("{transferring} active · {pending} uploads waiting"));

        if page == "overview" {
            let speed = e.speed;
            self.stat_fps.set_label(&e.files_per_sec.to_string());
            self.stat_speed.set_label(&format!("{}/s", human(speed)));
            self.stat_speed_sub.set_label(&format!("{} transferred this session", human(e.core["bytes"].as_f64().unwrap_or(0.0))));
            self.stat_streams.set_label(&transferring.to_string());
            let used = e.vfs.get(&a.remote).and_then(|v| v["diskCache"]["bytesUsed"].as_f64()).unwrap_or(0.0);
            let max = e.cfg.cache_max_size_gib as f64 * 1024f64.powi(3);
            self.stat_cache.set_label(&format!("{} / {} GiB", human(used), e.cfg.cache_max_size_gib));
            self.cache_bar.set_fraction((used / max).min(1.0));
            let files = e.vfs.get(&a.remote).and_then(|v| v["diskCache"]["files"].as_u64()).unwrap_or(0);
            self.stat_cache_sub.set_label(&format!("{files} files · max age {} h", e.cfg.cache_max_age_hours));
            self.stat_errors.set_label(&e.core["errors"].as_u64().unwrap_or(0).to_string());
            self.stat_errors_sub.set_label(e.core["lastError"].as_str().unwrap_or("No errors"));
            self.recent.remove_all();
            for t in e.transferred.iter().rev().take(6) {
                self.recent.append(&done_row(t));
            }
            if e.transferred.is_empty() {
                self.recent.append(&placeholder("No transfers yet"));
            }
        } else if page == "transfers" {
            self.active.remove_all();
            for t in e.core["transferring"].as_array().into_iter().flatten() {
                let (bytes, size, speed) = (t["bytes"].as_f64().unwrap_or(0.0), t["size"].as_f64().unwrap_or(0.0), t["speed"].as_f64().unwrap_or(0.0));
                let eta = t["eta"].as_u64().map_or(String::new(), |s| format!(" · {s} s left"));
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(t["name"].as_str().unwrap_or("?")).as_str())
                    .subtitle(format!("{} of {} · {}/s{eta}", human(bytes), human(size), human(speed)))
                    .build();
                let bar = gtk::ProgressBar::builder().fraction(t["percentage"].as_f64().unwrap_or(0.0) / 100.0).valign(gtk::Align::Center).width_request(140).build();
                row.add_suffix(&bar);
                self.active.append(&row);
            }
            if transferring == 0 {
                self.active.append(&placeholder(if e.paused { "Paused" } else { "Nothing is transferring" }));
            }
            self.done.remove_all();
            for t in e.transferred.iter().rev().take(25) {
                self.done.append(&done_row(t));
            }
            if e.transferred.is_empty() {
                self.done.append(&placeholder("Nothing yet"));
            }
        }
    }
}

fn done_row(t: &Value) -> adw::ActionRow {
    let err = t["error"].as_str().unwrap_or("");
    let when = t["completed_at"].as_str().and_then(|s| s.get(11..16)).unwrap_or("");
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(t["name"].as_str().unwrap_or("?")).as_str())
        .subtitle(glib::markup_escape_text(&if err.is_empty() { format!("{} · {when}", human(t["size"].as_f64().unwrap_or(0.0))) } else { err.to_owned() }).as_str())
        .build();
    row.add_suffix(&if err.is_empty() { pill("Done", "success") } else { pill("Error", "error") });
    row
}

fn autostart_file() -> std::path::PathBuf {
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
