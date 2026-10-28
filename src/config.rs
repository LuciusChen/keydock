//! The key -> application table, read from ~/.config/keydock/config.toml.

use serde::Deserialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct AppEntry {
    /// A single lowercase letter, used as Super+<key>.
    pub key: String,
    /// Wayland app_id (or X11 WM_CLASS). "emacs" also matches "emacs-32-0-50".
    pub app_id: String,
    /// Shell command that launches the app when no window exists.
    pub exec: Option<String>,
    /// Display name for the overlay; defaults to the desktop entry name.
    pub name: Option<String>,
}

impl AppEntry {
    pub fn matches(&self, app_id: &str) -> bool {
        app_id_matches(&self.app_id, app_id)
    }
}

/// Exact match, or a versioned id such as "emacs-32-0-50" for "emacs".
pub fn app_id_matches(wanted: &str, actual: &str) -> bool {
    let (w, a) = (wanted.to_lowercase(), actual.to_lowercase());
    a == w || a.strip_prefix(&w).is_some_and(|rest| rest.starts_with('-'))
}

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// Font family for the overlay, e.g. "Inter" or "sans".
    pub font: Option<String>,
    #[serde(default, rename = "app")]
    pub apps: Vec<AppEntry>,
}

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
        base.join("keydock/config.toml")
    }

    /// Reads the config; a missing or broken file yields an empty table and an error message.
    pub fn load() -> (Config, Option<String>) {
        let path = Self::path();
        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => (cfg, None),
                Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(_) => (Config::default(), None),
        }
    }

    pub fn by_key(&self, key: &str) -> Option<&AppEntry> {
        self.apps.iter().find(|a| a.key.eq_ignore_ascii_case(key))
    }

    pub fn by_app_id(&self, app_id: &str) -> Option<&AppEntry> {
        self.apps.iter().find(|a| a.matches(app_id))
    }

    /// Appends a binding as text so existing comments and layout survive.
    pub fn append(key: &str, app_id: &str, exec: Option<&str>) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut block = format!("\n[[app]]\nkey = {}\napp_id = {}\n", quote(key), quote(app_id));
        if let Some(exec) = exec {
            block.push_str(&format!("exec = {}\n", quote(exec)));
        }
        OpenOptions::new().create(true).append(true).open(path)?.write_all(block.as_bytes())
    }
}

fn quote(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}
