//! User configuration, stored as TOML in `~/.config/rclover/config.toml`.

use gtk::glib;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How a folder (and everything below it) is kept on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Cloud only: downloaded into the VFS cache when opened.
    OnDemand,
    /// Always offline: kept as an offline copy under the mount point (`engine::offline`).
    Pinned,
    /// Hidden from the mount and never transferred.
    Excluded,
}

/// What happens to a file in an offline folder that changed both on this device and in the
/// cloud since the last sync.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictPolicy {
    /// The newer version keeps the name; the other is kept as "name (conflict 1).ext".
    #[default]
    KeepBoth,
    /// The newer version wins; the other is deleted.
    Newer,
    /// This device's version wins; the cloud's is deleted.
    ThisDevice,
    /// The cloud's version wins; this device's is deleted.
    Cloud,
}

impl ConflictPolicy {
    pub const ALL: [ConflictPolicy; 4] = [Self::KeepBoth, Self::Newer, Self::ThisDevice, Self::Cloud];

    pub fn label(self) -> &'static str {
        match self {
            Self::KeepBoth => "Keep Both",
            Self::Newer => "Keep the Newer",
            Self::ThisDevice => "This Device Wins",
            Self::Cloud => "Cloud Wins",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::KeepBoth => "The newer version keeps the name; the other is kept next to it as “name (conflict 1).ext”. Nothing is lost.",
            Self::Newer => "The newer version is kept; the older one is deleted.",
            Self::ThisDevice => "The version on this device is kept; the cloud's is deleted.",
            Self::Cloud => "The cloud's version is kept; the one on this device is deleted.",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    /// rclone remote name, without the trailing colon.
    pub remote: String,
    pub mount: PathBuf,
    #[serde(default = "yes")]
    pub background_sync: bool,
    /// Per-path overrides, relative to the remote root ("" is the root).
    #[serde(default)]
    pub modes: BTreeMap<String, Mode>,
}

fn yes() -> bool {
    true
}

impl Account {
    pub fn new(remote: &str) -> Self {
        Account {
            remote: remote.to_owned(),
            mount: glib::home_dir().join("Cloud").join(remote),
            background_sync: true,
            modes: BTreeMap::new(),
        }
    }

    /// True if a folder strictly inside `rel` is pinned.
    pub fn has_pinned_below(&self, rel: &str) -> bool {
        let rel = rel.trim_matches('/');
        let prefix = if rel.is_empty() { String::new() } else { format!("{rel}/") };
        self.modes.iter().any(|(p, m)| *m == Mode::Pinned && p.len() > prefix.len() && p.starts_with(&prefix))
    }

    /// False when the whole drive is pinned (a full offline mirror).
    pub fn on_demand(&self) -> bool {
        self.modes.get("") != Some(&Mode::Pinned)
    }

    pub fn fs(&self) -> String {
        format!("{}:", self.remote)
    }

    /// True if something strictly inside `rel` has its own mode, different from `rel`'s.
    pub fn is_mixed(&self, rel: &str) -> bool {
        let rel = rel.trim_matches('/');
        let own = self.mode_of(rel);
        let prefix = if rel.is_empty() { String::new() } else { format!("{rel}/") };
        self.modes.iter().any(|(p, m)| *m != own && p.len() > prefix.len() && p.starts_with(&prefix))
    }

    /// The mode of `rel`, inherited from its nearest configured ancestor.
    pub fn mode_of(&self, rel: &str) -> Mode {
        let mut p = rel.trim_matches('/');
        loop {
            if let Some(m) = self.modes.get(p) {
                return *m;
            }
            if p.is_empty() {
                return Mode::OnDemand;
            }
            p = p.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }

    /// Nearest strict ancestor of `rel` that is [`Mode::Excluded`], if any.
    pub fn excluded_ancestor(&self, rel: &str) -> Option<String> {
        let mut p = rel.trim_matches('/');
        while !p.is_empty() {
            p = p.rsplit_once('/').map_or("", |(parent, _)| parent);
            if self.modes.get(p) == Some(&Mode::Excluded) {
                return Some(p.to_owned());
            }
        }
        None
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    pub transfers: u32,
    pub checkers: u32,
    pub cache_max_size_gib: u32,
    pub cache_max_age_hours: u32,
    pub no_modtime: bool,
    pub ignore: Vec<String>,
    pub rc_port: u16,
    /// How offline copies settle a file changed in both places.
    pub conflicts: ConflictPolicy,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            accounts: Vec::new(),
            transfers: 4,
            checkers: 8,
            cache_max_size_gib: 20,
            cache_max_age_hours: 168,
            no_modtime: true,
            ignore: ["node_modules/**", "target/**", "_deps/**", "*.tmp", ".~lock.*#"]
                .map(String::from)
                .to_vec(),
            rc_port: 5572,
            conflicts: ConflictPolicy::KeepBoth,
        }
    }
}

pub fn path() -> PathBuf {
    glib::user_config_dir().join("rclover").join("config.toml")
}

/// Loads the config. A file that does not parse is moved aside rather than overwritten later.
pub fn load() -> Result<Config, String> {
    let p = path();
    let Ok(text) = fs::read_to_string(&p) else {
        return Ok(Config::default());
    };
    toml::from_str(&text).or_else(|e| {
        let backup = p.with_extension("toml.bak");
        fs::rename(&p, &backup).map_err(|r| format!("{}: {e}; could not move it aside: {r}", p.display()))?;
        eprintln!("rclover: {} is invalid ({e}); moved to {}", p.display(), backup.display());
        Ok(Config::default())
    })
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let p = path();
    fs::create_dir_all(p.parent().unwrap_or(Path::new(".")))?;
    let tmp = p.with_extension("toml.tmp");
    fs::write(&tmp, toml::to_string_pretty(cfg).expect("config serializes"))?;
    fs::rename(tmp, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_is_inherited_from_nearest_ancestor() {
        let mut a = Account::new("gdrive");
        a.modes.insert("Projects".into(), Mode::Pinned);
        a.modes.insert("Projects/web/node_modules".into(), Mode::Excluded);
        assert_eq!(a.mode_of("Documents/a.txt"), Mode::OnDemand);
        assert_eq!(a.mode_of("Projects"), Mode::Pinned);
        assert_eq!(a.mode_of("Projects/web/src/main.rs"), Mode::Pinned);
        assert_eq!(a.mode_of("Projects/web/node_modules/x/y.js"), Mode::Excluded);
        assert!(a.has_pinned_below(""));
        assert!(!a.has_pinned_below("Projects"));
        assert!(!a.has_pinned_below("Documents"));
        assert!(a.is_mixed("Projects"), "web/node_modules is excluded inside a pinned folder");
        assert!(!a.is_mixed("Projects/web/src"));
        assert!(!a.is_mixed("Documents"));
        assert_eq!(a.excluded_ancestor("Projects/web/node_modules/pkg"), Some("Projects/web/node_modules".into()));
        assert_eq!(a.excluded_ancestor("Projects/web/node_modules"), None);
        assert_eq!(a.excluded_ancestor("Projects/web/src"), None);
        a.modes.insert(String::new(), Mode::Pinned);
        assert_eq!(a.mode_of("Documents/a.txt"), Mode::Pinned);
    }

    #[test]
    fn config_round_trips() {
        let mut c = Config::default();
        c.accounts.push(Account::new("gdrive"));
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back.accounts[0].remote, "gdrive");
        assert_eq!(back.ignore, c.ignore);
    }
}
