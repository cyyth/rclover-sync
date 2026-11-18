//! User configuration, stored as TOML in `~/.config/nuvolo/config.toml`.

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
    /// Always offline: Nuvolo reads it into the VFS cache and keeps it warm.
    Pinned,
    /// Hidden from the mount and never transferred.
    Excluded,
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

    /// False when the whole drive is pinned (a full offline mirror).
    pub fn on_demand(&self) -> bool {
        self.modes.get("") != Some(&Mode::Pinned)
    }

    pub fn fs(&self) -> String {
        format!("{}:", self.remote)
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
        }
    }
}

pub fn path() -> PathBuf {
    glib::user_config_dir().join("nuvolo").join("config.toml")
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
        eprintln!("nuvolo: {} is invalid ({e}); moved to {}", p.display(), backup.display());
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
