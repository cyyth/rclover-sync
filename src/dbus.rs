//! D-Bus interface used by the Nautilus extension. See docs/dbus.rst for the contract.

use crate::engine::Shared;
use crate::rclone;
use gtk::gio::prelude::*;
use gtk::{gio, glib};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const IFACE: &str = "io.github.cyythanapong_netizen.Nuvolo1";
pub const PATH: &str = "/io/github/cyythanapong_netizen/Nuvolo";
const ERROR: &str = "io.github.cyythanapong_netizen.Nuvolo1.Error";

const XML: &str = r#"<node>
  <interface name="io.github.cyythanapong_netizen.Nuvolo1">
    <method name="GetMounts"><arg type="as" direction="out"/></method>
    <method name="GetStates"><arg name="paths" type="as" direction="in"/><arg type="a{ss}" direction="out"/></method>
    <method name="GetDirStates"><arg name="dir" type="s" direction="in"/><arg type="a{ss}" direction="out"/></method>
    <method name="Pin"><arg name="paths" type="as" direction="in"/></method>
    <method name="Unpin"><arg name="paths" type="as" direction="in"/></method>
    <method name="SyncNow"><arg name="paths" type="as" direction="in"/></method>
    <method name="GetShareLink"><arg name="path" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="GetWebUrl"><arg name="path" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <signal name="StatesChanged"><arg name="paths" type="as"/></signal>
  </interface>
</node>"#;

/// Exports the interface on the application's own bus connection (its name is the app ID).
pub fn export(conn: &gio::DBusConnection, e: Shared) {
    let node = gio::DBusNodeInfo::for_xml(XML).expect("valid introspection XML");
    let iface = node.lookup_interface(IFACE).expect("interface present in XML");
    let c = conn.clone();
    let res = conn
        .register_object(PATH, &iface)
        .method_call(move |_, _, _, _, method, params, inv| handle(&e, &c, method, params, inv))
        .build();
    if let Err(err) = res {
        eprintln!("nuvolo: could not export D-Bus interface: {err}");
    }
}

pub fn emit_changed(conn: &gio::DBusConnection, paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    if let Err(err) = conn.emit_signal(None, PATH, IFACE, "StatesChanged", Some(&(paths,).to_variant())) {
        eprintln!("nuvolo: StatesChanged: {err}");
    }
}

fn handle(e: &Shared, conn: &gio::DBusConnection, method: &str, params: glib::Variant, inv: gio::DBusMethodInvocation) {
    let paths = || params.get::<(Vec<String>,)>().map(|t| t.0).unwrap_or_default();
    let path = || params.get::<(String,)>().map(|t| t.0).unwrap_or_default();
    match method {
        "GetMounts" => {
            let m: Vec<String> = e.borrow().cfg.accounts.iter().map(|a| a.mount.to_string_lossy().into_owned()).collect();
            inv.return_value(Some(&(m,).to_variant()));
        }
        "GetStates" | "GetDirStates" => {
            // Looking at files goes through the mount, which can wait on the network: do it
            // off the main thread, so neither Nuvolo nor GNOME Files freezes meanwhile.
            let (paths, dir) = if method == "GetStates" { (paths(), None) } else { (Vec::new(), Some(PathBuf::from(path()))) };
            if dir.as_ref().is_some_and(|d| e.borrow().locate(d).is_none()) {
                return inv.return_value(Some(&(HashMap::<String, String>::new(),).to_variant()));
            }
            let e = e.clone();
            glib::spawn_future_local(async move {
                let entries = gio::spawn_blocking(move || match dir {
                    Some(d) => list_dir(&d),
                    None => paths.into_iter().map(|p| (Path::new(&p).is_dir(), PathBuf::from(p))).collect(),
                })
                .await
                .unwrap_or_default();
                let e = e.borrow();
                let map: HashMap<String, String> = entries
                    .iter()
                    .filter_map(|(is_dir, p)| e.state(p, *is_dir).map(|s| (p.to_string_lossy().into_owned(), s.as_str().to_owned())))
                    .collect();
                inv.return_value(Some(&(map,).to_variant()));
            });
        }
        "Pin" | "Unpin" | "SyncNow" => {
            let ps: Vec<PathBuf> = paths().into_iter().map(PathBuf::from).collect();
            {
                let mut e = e.borrow_mut();
                for p in &ps {
                    match method {
                        "Pin" => e.pin(p),
                        "Unpin" => e.free(p),
                        _ => e.sync_now(p),
                    }
                }
            }
            emit_changed(conn, &ps);
            inv.return_value(None);
        }
        "GetShareLink" | "GetWebUrl" => {
            let Some((client, remote, rel)) = e.borrow().link_job(Path::new(&path())) else {
                return inv.return_dbus_error(ERROR, "not inside a Nuvolo mount");
            };
            let web = method == "GetWebUrl";
            glib::spawn_future_local(async move {
                let res = gio::spawn_blocking(move || {
                    if web { rclone::web_url(&client, &remote, &rel) } else { rclone::public_link(&client, &remote, &rel) }
                })
                .await
                .unwrap_or_else(|_| Err("link lookup crashed".to_owned()));
                match res {
                    Ok(url) => inv.return_value(Some(&(url,).to_variant())),
                    Err(msg) => inv.return_dbus_error(ERROR, &msg),
                }
            });
        }
        _ => inv.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
    }
}

/// Entries of `dir` and whether each is a folder. The type comes with the listing, so this
/// costs one directory read and no stat per file.
fn list_dir(dir: &Path) -> Vec<(bool, PathBuf)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| (e.file_type().is_ok_and(|t| t.is_dir()), e.path()))
        .collect()
}
