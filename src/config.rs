//! The key -> application table in ~/.config/keydock/config.toml.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppEntry {
    /// A letter; the compositor binds a modifier plus this letter to `keydock key <letter>`.
    pub key: char,
    /// Wayland app_id or X11 WM_CLASS. "emacs" also matches "emacs-32-0-50".
    pub app_id: String,
    /// Shell command that launches the app when it has no window.
    pub exec: Option<String>,
    /// Name shown in the overlay instead of the desktop entry's.
    pub name: Option<String>,
}

/// Exact match, or a versioned id such as "emacs-32-0-50" for "emacs".
pub fn app_id_matches(wanted: &str, actual: &str) -> bool {
    let (wanted, actual) = (wanted.to_lowercase(), actual.to_lowercase());
    actual == wanted
        || actual
            .strip_prefix(&wanted)
            .is_some_and(|rest| rest.starts_with('-'))
}

/// What pressing the key of the focused app does.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hide {
    /// Minimize where the compositor supports it, otherwise `Previous`.
    #[default]
    Auto,
    Minimize,
    /// Focus another app's most recent window, like Cmd+Tab.
    Previous,
}

const DEFAULT_WARP: &str = "super+ctrl+alt+shift+p";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Overlay font family.
    pub font: Option<String>,
    /// Overlay background opacity, 0.0 to 1.0.
    opacity: Option<f64>,
    #[serde(default)]
    pub hide: Hide,
    /// Modifier name shown before the letter in the overlay.
    key_label: Option<String>,
    /// Chord that makes the compositor warp the pointer; empty disables it.
    warp: Option<String>,
    #[serde(default, rename = "app")]
    pub apps: Vec<AppEntry>,
}

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            });
        base.join("keydock/config.toml")
    }

    pub fn load() -> Result<Config, String> {
        Self::load_from(&Self::path())
    }

    /// A missing file is an empty config; a broken one is an error.
    pub fn load_from(path: &Path) -> Result<Config, String> {
        let Ok(text) = fs::read_to_string(path) else {
            return Ok(Config::default());
        };
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut seen = Vec::new();
        for app in &cfg.apps {
            let key = app.key.to_ascii_lowercase();
            if !key.is_ascii_lowercase() {
                return Err(format!(
                    "key {:?} for {} is not a letter",
                    app.key, app.app_id
                ));
            }
            if seen.contains(&key) {
                return Err(format!("key {key:?} is bound more than once"));
            }
            seen.push(key);
        }
        Ok(cfg)
    }

    pub fn key_label(&self) -> &str {
        self.key_label.as_deref().unwrap_or("Super")
    }

    pub fn opacity(&self) -> f64 {
        self.opacity.unwrap_or(1.0).clamp(0.0, 1.0)
    }

    /// The warp chord, or None when disabled.
    pub fn warp(&self) -> Option<&str> {
        let chord = self.warp.as_deref().unwrap_or(DEFAULT_WARP).trim();
        (!chord.is_empty() && !chord.eq_ignore_ascii_case("none")).then_some(chord)
    }

    pub fn by_key(&self, key: char) -> Option<&AppEntry> {
        self.apps.iter().find(|a| a.key.eq_ignore_ascii_case(&key))
    }

    pub fn by_app_id(&self, app_id: &str) -> Option<&AppEntry> {
        self.apps.iter().find(|a| app_id_matches(&a.app_id, app_id))
    }

    /// Appends a binding as text, so comments and layout in the file survive.
    pub fn append_to(
        path: &Path,
        key: char,
        app_id: &str,
        exec: Option<&str>,
    ) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let quote = |s: &str| toml::Value::String(s.to_owned()).to_string();
        let mut block = format!(
            "\n[[app]]\nkey = {}\napp_id = {}\n",
            quote(&key.to_string()),
            quote(app_id)
        );
        if let Some(exec) = exec {
            block.push_str(&format!("exec = {}\n", quote(exec)));
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(block.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let cfg = Config::parse(
            r#"
            font = "Inter"
            hide = "previous"
            key_label = "Hyper"
            warp = "alt+super+f12"
            opacity = 0.8

            [[app]]
            key = "c"
            app_id = "kitty"
            exec = "kitty"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.hide, Hide::Previous);
        assert_eq!(cfg.key_label(), "Hyper");
        assert_eq!(cfg.warp(), Some("alt+super+f12"));
        assert_eq!(cfg.opacity(), 0.8);
        assert_eq!(cfg.by_key('C').unwrap().exec.as_deref(), Some("kitty"));
        assert!(cfg.by_app_id("Kitty").is_some());
    }

    #[test]
    fn defaults() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.hide, Hide::Auto);
        assert_eq!(cfg.key_label(), "Super");
        assert_eq!(cfg.warp(), Some(DEFAULT_WARP));
        assert_eq!(cfg.opacity(), 1.0);
        assert_eq!(Config::parse("opacity = 3.0").unwrap().opacity(), 1.0);
    }

    #[test]
    fn empty_warp_disables_it() {
        assert_eq!(Config::parse(r#"warp = """#).unwrap().warp(), None);
        assert_eq!(Config::parse(r#"warp = "none""#).unwrap().warp(), None);
    }

    #[test]
    fn rejects_bad_keys_and_typos() {
        let app = |key: &str| format!("[[app]]\nkey = \"{key}\"\napp_id = \"a\"\n");
        assert!(Config::parse(&app("1")).is_err());
        assert!(Config::parse(&app("ab")).is_err());
        assert!(Config::parse(&format!("{}{}", app("c"), app("C"))).is_err());
        assert!(Config::parse("hide = \"sometimes\"").is_err());
        assert!(Config::parse("fnot = \"Inter\"").is_err());
    }

    #[test]
    fn matches_versioned_app_ids() {
        assert!(app_id_matches("emacs", "emacs-32-0-50"));
        assert!(app_id_matches("QQ", "qq"));
        assert!(!app_id_matches("emacs", "emacsclient"));
    }

    #[test]
    fn appended_binding_parses_and_keeps_comments() {
        let dir = std::env::temp_dir().join(format!("keydock-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "# my apps\n").unwrap();
        Config::append_to(
            &path,
            'e',
            "org.gnome.Nautilus",
            Some("gtk-launch \"files\""),
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(text.starts_with("# my apps\n"));
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(
            cfg.by_key('e').unwrap().exec.as_deref(),
            Some("gtk-launch \"files\"")
        );
    }
}
