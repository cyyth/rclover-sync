//! The Nuvolo window: an account sidebar and four pages. Built on first use and then kept
//! (closing only hides it); [`refresh`] redraws the visible page once a second.
//!
//! - [`accounts`]: the sidebar: listing, selecting and adding accounts.
//! - [`overview`]: mount status (with Retry), sync switches, live statistics.
//! - [`folders`]: per-folder On-Demand / Offline / Excluded.
//! - [`transfers`]: running and finished transfers.
//! - [`prefs`]: preferences, OAuth, reconnect, ignore patterns.
//! - [`home`]: the Home Folders rows inside Preferences.
//! - [`help`]: the Help dialog (GNOME Files icons, conflicts).
//! - [`widgets`]: small widget helpers shared by the pages.

mod accounts;
mod folders;
mod help;
mod home;
mod overview;
mod prefs;
mod transfers;
mod widgets;

use crate::engine::Shared;
use adw::prelude::*;
use gtk::{gio, glib};
use overview::Overview;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use transfers::Transfers;
use widgets::*;

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

/// True while the main window exists and is visible.
pub fn is_visible() -> bool {
    UI.with(|u| u.borrow().as_ref().is_some_and(|ui| ui.window.is_visible()))
}

/// Called every second by the engine tick.
pub fn refresh() {
    if let Some(ui) = UI.with(|u| u.borrow().clone()) {
        if ui.window.is_visible() {
            ui.tick();
        }
    }
}

/// Redraws the Home Folders rows, if the window exists (a move started, progressed or ended).
pub fn refresh_home() {
    if let Some(ui) = UI.with(|u| u.borrow().clone()) {
        ui.rebuild_home();
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
    // Window chrome
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    split: adw::NavigationSplitView,
    title: adw::WindowTitle,
    banner: adw::Banner,
    stack: adw::ViewStack,
    accounts: gtk::ListBox,
    nav: gtk::ListBox,
    /// The selected account.
    acct: Cell<usize>,
    /// Set while switches are updated from code, so their handlers do nothing.
    updating: Cell<bool>,
    // Pages
    ov: Overview,
    tr: Transfers,
    folders: gtk::ListBox,
    client_row: adw::ActionRow,
    ignore: gtk::ListBox,
    home: gtk::ListBox,
    login: adw::SwitchRow,
    /// What the activity lists show: (transfer list generation, page). Redrawn only on change.
    shown: RefCell<(u64, String)>,
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
        menu.append(Some("Help"), Some("app.help"));
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
        let help_btn = gtk::Button::builder().icon_name("help-browser-symbolic").tooltip_text("Help: icons in GNOME Files, conflicts").action_name("app.help").build();
        help_btn.add_css_class("flat");
        header.pack_end(&help_btn);
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
            nav.append(&gtk::ListBoxRow::builder().child(&row).name(id).build());
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
            ov: Overview::new(),
            tr: Transfers::new(),
            folders: boxed(),
            client_row: adw::ActionRow::builder().title("OAuth Client").build(),
            ignore: boxed(),
            home: boxed(),
            login: adw::SwitchRow::builder()
                .title("Start at Login")
                .subtitle("Mount your drives in the background when you log in. Needed for home folders kept in the cloud.")
                .active(prefs::autostart_file().exists())
                .build(),
            shown: RefCell::new((u64::MAX, String::new())),
        });
        ui.stack.add_named(&ui.overview_page(), Some("overview"));
        ui.stack.add_named(&ui.folders_page(), Some("folders"));
        ui.stack.add_named(&ui.transfers_page(), Some("transfers"));
        ui.stack.add_named(&ui.prefs_page(), Some("prefs"));
        ui.wire();
        ui.rebuild_accounts();
        ui.select_account(0);
        ui.nav.select_row(ui.nav.row_at_index(0).as_ref());
        ui
    }

    /// Window-wide signal handlers. Each page wires its own widgets when it builds them.
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
        self.banner.connect_button_clicked(move |_| ui.client_id_dialog());
    }

    /// Switches the window to one of the sidebar pages ("overview", "prefs", ...).
    pub fn show_page(&self, id: &str) {
        if let Some(i) = PAGES.iter().position(|p| p.0 == id) {
            self.nav.select_row(self.nav.row_at_index(i as i32).as_ref());
        }
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(&glib::markup_escape_text(msg)));
    }

    /// A toast for a saved setting that needs a remount, with a button to remount now.
    fn toast_remount(self: &Rc<Self>, msg: &str) {
        let t = adw::Toast::builder().title(glib::markup_escape_text(msg).as_str()).button_label("Remount Now").build();
        let ui = self.clone();
        t.connect_button_clicked(move |_| {
            let n = ui.e.borrow().cfg.accounts.len();
            let errors: Vec<String> = (0..n).filter_map(|i| ui.e.borrow_mut().remount(i).err()).collect();
            ui.toast(&if errors.is_empty() { "Remounting…".to_owned() } else { errors.join("; ") });
        });
        self.toasts.add_toast(t);
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

    /// Redraws what the visible page shows from the engine's latest state.
    fn tick(self: &Rc<Self>) {
        if self.e.borrow().cfg.accounts.get(self.acct.get()).is_none() {
            return;
        }
        self.tick_mount();
        let page = self.stack.visible_child_name().unwrap_or_default();
        let gen = self.e.borrow().transferred_gen;
        // Lists of finished transfers change every few seconds at most: redraw only then.
        let fresh = *self.shown.borrow() != (gen, page.to_string());
        if fresh {
            *self.shown.borrow_mut() = (gen, page.to_string());
        }
        match page.as_str() {
            "overview" => self.tick_overview(fresh),
            "transfers" => self.tick_transfers(fresh),
            "prefs" if !self.e.borrow().jobs.is_empty() => self.rebuild_home(), // live move progress
            _ => {}
        }
    }
}
