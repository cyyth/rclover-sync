//! Small widget helpers shared by the pages.

use crate::config::Mode;
use adw::prelude::*;
use gtk::{glib, pango};
use serde_json::Value;

pub(super) fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

pub(super) fn boxed() -> gtk::ListBox {
    let l = gtk::ListBox::new();
    l.add_css_class("boxed-list");
    l.set_selection_mode(gtk::SelectionMode::None);
    l
}

pub(super) fn group(title: &str, desc: Option<&str>) -> adw::PreferencesGroup {
    // Group titles and descriptions are markup: escape "&" and friends.
    let g = adw::PreferencesGroup::builder().title(glib::markup_escape_text(title).as_str()).build();
    g.set_description(desc.map(glib::markup_escape_text).as_deref());
    g
}

pub(super) fn set_pill(l: &gtk::Label, text: &str, kind: &str) {
    if l.label().as_str() == text && l.has_css_class(kind) {
        return;
    }
    l.set_label(text);
    for k in ["success", "warning", "error"] {
        l.remove_css_class(k);
    }
    l.add_css_class(kind);
}

pub(super) fn pill(text: &str, kind: &str) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.add_css_class("pill-badge");
    l.set_valign(gtk::Align::Center);
    set_pill(&l, text, kind);
    l
}

pub(super) fn placeholder(text: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder().title(text).build();
    r.add_css_class("dim-label");
    r
}

pub(super) fn stat_card(title: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
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

pub(super) fn human(b: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let (mut b, mut i) = (b, 0);
    while b >= 1024.0 && i < units.len() - 1 {
        b /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b:.0} B") } else { format!("{b:.1} {}", units[i]) }
}

pub(super) fn mode_label(m: Mode) -> &'static str {
    match m {
        Mode::OnDemand => "On-Demand · cloud only until opened",
        Mode::Pinned => "Always Offline · kept in the local cache",
        Mode::Excluded => "Excluded · hidden and never synced",
    }
}

pub(super) fn button(text: &str) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.set_valign(gtk::Align::Center);
    b
}

pub(super) fn done_row(t: &Value) -> adw::ActionRow {
    let err = t["error"].as_str().unwrap_or("");
    let when = t["completed_at"].as_str().and_then(|s| s.get(11..16)).unwrap_or("");
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(t["name"].as_str().unwrap_or("?")).as_str())
        .subtitle(glib::markup_escape_text(&if err.is_empty() { format!("{} · {when}", human(t["size"].as_f64().unwrap_or(0.0))) } else { err.to_owned() }).as_str())
        .build();
    row.add_suffix(&if err.is_empty() { pill("Done", "success") } else { pill("Error", "error") });
    row
}
