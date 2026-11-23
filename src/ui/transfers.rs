//! The Transfers & Queue page: running and recently finished transfers.

use super::widgets::*;
use super::Ui;
use adw::prelude::*;
use gtk::glib;
use serde_json::Value;
use std::cell::RefCell;

pub(super) struct Transfers {
    active: gtk::ListBox,
    done: gtk::ListBox,
    last_active: RefCell<Option<(Vec<Value>, bool)>>,
}

impl Transfers {
    pub fn new() -> Transfers {
        Transfers {
            active: boxed(),
            done: boxed(),
            last_active: RefCell::new(None),
        }
    }
}

impl Ui {
    pub(super) fn transfers_page(&self) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();
        let g = group("Active", Some("Uploads and downloads running now."));
        g.add(&self.tr.active);
        page.add(&g);
        let g = group("Recently Completed", None);
        g.add(&self.tr.done);
        page.add(&g);
        page
    }

    /// Running transfers are redrawn only when the list or pause state changed; finished ones
    /// only when `fresh`.
    pub(super) fn tick_transfers(&self, fresh: bool) {
        let (tr, e) = (&self.tr, self.e.borrow());
        let running = e.core["transferring"].as_array().cloned().unwrap_or_default();
        let key = (running, e.paused);
        if tr.last_active.borrow().as_ref() != Some(&key) {
            tr.active.remove_all();
            for t in &key.0 {
                let (bytes, size, speed) = (t["bytes"].as_f64().unwrap_or(0.0), t["size"].as_f64().unwrap_or(0.0), t["speed"].as_f64().unwrap_or(0.0));
                let eta = t["eta"].as_u64().map_or(String::new(), |s| format!(" · {s} s left"));
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(t["name"].as_str().unwrap_or("?")).as_str())
                    .subtitle(format!("{} of {} · {}/s{eta}", human(bytes), human(size), human(speed)))
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("network-transmit-receive-symbolic"));
                let bar = gtk::ProgressBar::builder().fraction(t["percentage"].as_f64().unwrap_or(0.0) / 100.0).valign(gtk::Align::Center).width_request(140).build();
                row.add_suffix(&bar);
                tr.active.append(&row);
            }
            if key.0.is_empty() {
                tr.active.append(&placeholder(if key.1 { "Paused" } else { "Nothing is transferring" }));
            }
            *tr.last_active.borrow_mut() = Some(key);
        }
        if fresh {
            tr.done.remove_all();
            for t in e.transferred.iter().rev().take(25) {
                tr.done.append(&done_row(t));
            }
            if e.transferred.is_empty() {
                tr.done.append(&placeholder("Nothing yet"));
            }
        }
    }
}
