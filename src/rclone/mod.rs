//! Everything that talks to rclone. rclover never runs rclone commands directly: it starts one
//! private `rclone rcd` ([`daemon`]) and drives it over its JSON remote-control API ([`client`]).
//!
//! - [`client`]: the HTTP client for the remote-control API.
//! - [`daemon`]: starting and stopping the `rclone rcd` process.
//! - [`mount`]: mounting a remote, and reading the kernel's mount table.
//! - [`bisync`]: two-way sync between a local folder and the cloud (offline copies).
//! - [`links`]: share links and browser URLs.

pub(crate) mod bisync;
mod client;
mod daemon;
mod links;
mod mount;

pub use bisync::{bisync, mirror, wait_for_uploads, Scope};
pub use client::Client;
pub use daemon::{Rcd, RcloneCaps};
pub use links::{public_link, web_url};
pub use mount::{canonical_mount_path, is_mounted_path, lazy_unmount, mount, mounted_paths, probe_mount, MountHealth};
