//! The Help dialog: what the icons in GNOME Files mean, and how conflicts are settled.

use super::widgets::*;
use super::Ui;
use adw::prelude::*;
use gtk::glib;

/// The emblems the GNOME Files extension draws, and what each one means.
const EMBLEMS: [(&str, &str, &str); 6] = [
    ("emblem-nuvolo-cloud", "In the cloud", "Not on this device. Opening it downloads it, which needs the internet."),
    ("emblem-nuvolo-cached", "Downloaded", "Opened before, so a copy is in the on-demand cache. It may be cleared when the cache needs room."),
    ("emblem-nuvolo-pinned", "Always on this device", "Set to Offline: kept on this device, also when Nuvolo is not running."),
    ("emblem-nuvolo-partial", "Partly on this device", "A folder holding some files that are downloaded or kept offline."),
    ("emblem-nuvolo-syncing", "Syncing", "Uploading or downloading right now."),
    ("emblem-nuvolo-error", "Sync failed", "The last upload or download failed. Transfers & Queue shows why."),
];

impl Ui {
    pub fn help(&self) {
        let page = adw::PreferencesPage::new();

        let icons = group("Icons in GNOME Files", Some("Nuvolo marks files in your drives with these small icons."));
        let theme = gtk::IconTheme::for_display(&gtk::prelude::WidgetExt::display(&self.window));
        if !theme.has_icon(EMBLEMS[0].0) {
            let row = adw::ActionRow::builder()
                .title("The GNOME Files extension is not installed")
                .subtitle("Install nuvolo-nautilus to see these icons and a Nuvolo menu in GNOME Files.")
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
            icons.add(&row);
        }
        for (icon, title, what) in EMBLEMS {
            let row = adw::ActionRow::builder().title(title).subtitle(glib::markup_escape_text(what).as_str()).subtitle_lines(0).build();
            let img = gtk::Image::from_icon_name(icon);
            img.set_pixel_size(32);
            row.add_prefix(&img);
            icons.add(&row);
        }
        page.add(&icons);

        let policy = self.e.borrow().cfg.conflicts;
        let conflicts = group("When a File Changes in Two Places", None);
        for (title, what) in [
            (
                "Files in Offline folders",
                format!(
                    "If a file changed both on this device and in the cloud since the last sync: {} ({}). Change this in Preferences → Conflicts.",
                    policy.label(),
                    policy.describe()
                ),
            ),
            (
                "Files opened through the drive",
                "Saving a file in the mounted drive uploads it a few seconds later. If someone changed the same file in the cloud meanwhile, the last save wins.".to_owned(),
            ),
            (
                "Safety stops",
                "If a sync would delete more than half of an offline folder, or every file in it changed at once, Nuvolo stops and asks: click Sync Anyway on the Overview page if that was intended.".to_owned(),
            ),
        ] {
            conflicts.add(&adw::ActionRow::builder().title(title).subtitle(glib::markup_escape_text(&what).as_str()).subtitle_lines(0).build());
        }
        page.add(&conflicts);

        let more = group("More", None);
        let docs = adw::ActionRow::builder().title("Documentation").subtitle(crate::DOCS_URL).activatable(true).build();
        docs.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        docs.connect_activated(|_| {
            let _ = gtk::gio::AppInfo::launch_default_for_uri(crate::DOCS_URL, None::<&gtk::gio::AppLaunchContext>);
        });
        more.add(&docs);
        page.add(&more);

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&page));
        let d = adw::Dialog::builder().title("Help").content_width(560).content_height(640).child(&tv).build();
        d.present(Some(&self.window));
    }
}
