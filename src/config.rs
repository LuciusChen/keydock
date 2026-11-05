//! The key -> application table in ~/.config/keydock/config.toml.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// A layer of another tool to show with `keydock peek <name>`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub name: String,
    /// The key held for this layer: the title, and the key drawn highlighted.
    pub prefix: String,
    /// Text for each key of the keyboard, e.g. h = "←".
    pub keys: HashMap<String, String>,
    /// The same text in other languages, by language such as "zh_CN" or "zh".
    #[serde(default)]
    pub translations: HashMap<String, HashMap<String, String>>,
}

impl Layer {
    /// The key text for the first of `languages` it is translated to; a
    /// translation may cover only some keys.
    pub fn labels(&self, languages: &[String]) -> HashMap<String, String> {
        let translation = languages.iter().find_map(|lang| {
            let primary = lang.split('_').next().unwrap_or(lang);
            self.translations
                .get(lang)
                .or_else(|| self.translations.get(primary))
        });
        let mut labels = self.keys.clone();
        labels.extend(
            translation
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        labels
    }
}

/// Which layout to draw when an input device is connected.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyboardRule {
    /// Part of the device name, ignoring case.
    pub device: String,
    pub layout: String,
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
    #[serde(default)]
    pub hide: Hide,
    /// Modifier name shown before the letter in the overlay.
    key_label: Option<String>,
    /// Chord that makes the compositor warp the pointer; empty disables it.
    warp: Option<String>,
    /// Ignored: the overlay is always opaque now, but older configs still set it.
    #[serde(rename = "opacity")]
    _opacity: Option<toml::Value>,
    /// Keyboard layout to draw when no `keyboard` rule matches.
    layout: Option<String>,
    /// How long the trigger key is held before the peek shows.
    peek_delay_ms: Option<u64>,
    #[serde(default, rename = "keyboard")]
    keyboards: Vec<KeyboardRule>,
    #[serde(default, rename = "app")]
    pub apps: Vec<AppEntry>,
    #[serde(default, rename = "layer")]
    pub layers: Vec<Layer>,
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
        for (i, layer) in cfg.layers.iter().enumerate() {
            if layer.name == "off" {
                return Err("a layer cannot be named off".into());
            }
            if cfg.layers[..i].iter().any(|l| l.name == layer.name) {
                return Err(format!("layer {} is defined more than once", layer.name));
            }
        }
        Ok(cfg)
    }

    pub fn key_label(&self) -> &str {
        self.key_label.as_deref().unwrap_or("Super")
    }

    /// The warp chord, or None when disabled.
    pub fn warp(&self) -> Option<&str> {
        let chord = self.warp.as_deref().unwrap_or(DEFAULT_WARP).trim();
        (!chord.is_empty() && !chord.eq_ignore_ascii_case("none")).then_some(chord)
    }

    pub fn by_key(&self, key: char) -> Option<&AppEntry> {
        self.apps.iter().find(|a| a.key.eq_ignore_ascii_case(&key))
    }

    pub fn peek_delay(&self) -> Duration {
        Duration::from_millis(self.peek_delay_ms.unwrap_or(400))
    }

    /// The layout of the first rule matching a device, else the default one.
    pub fn layout_name(&self, devices: &[String]) -> &str {
        self.keyboards
            .iter()
            .find(|rule| {
                let wanted = rule.device.to_lowercase();
                devices.iter().any(|d| d.to_lowercase().contains(&wanted))
            })
            .map(|rule| rule.layout.as_str())
            .or(self.layout.as_deref())
            .unwrap_or("ansi")
    }

    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
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
    fn labels_follow_the_language() {
        let cfg = Config::parse(
            r#"
            [[layer]]
            name = "super"
            prefix = "Super"
            [layer.keys]
            q = "Close"
            f = "Fullscreen"
            [layer.translations.zh]
            q = "关闭"
            [layer.translations.zh_TW]
            q = "關閉"
            "#,
        )
        .unwrap();
        let layer = cfg.layer("super").unwrap();
        let q = |langs: &[&str]| {
            let langs: Vec<String> = langs.iter().map(|l| l.to_string()).collect();
            layer.labels(&langs)["q"].clone()
        };
        assert_eq!(q(&["de_DE"]), "Close");
        assert_eq!(q(&["zh_CN"]), "关闭");
        assert_eq!(q(&["zh_TW"]), "關閉");
        assert_eq!(q(&["fr", "zh_CN"]), "关闭");
        assert_eq!(layer.labels(&["zh_CN".into()])["f"], "Fullscreen");
    }

    #[test]
    fn picks_the_layout_of_a_connected_device() {
        let cfg = Config::parse(
            r#"
            layout = "mine"
            [[keyboard]]
            device = "corne"
            layout = "corne"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.layout_name(&["foostan Corne v4".into()]), "corne");
        assert_eq!(cfg.layout_name(&["AT keyboard".into()]), "mine");
        assert_eq!(Config::default().layout_name(&[]), "ansi");
    }

    #[test]
    fn rejects_bad_layers() {
        let layer =
            |name: &str| format!("[[layer]]\nname = \"{name}\"\nprefix = \"x\"\nkeys = {{}}\n");
        assert!(Config::parse(&layer("off")).is_err());
        assert!(Config::parse(&(layer("a") + &layer("a"))).is_err());
    }

    #[test]
    fn parses_full_config() {
        let cfg = Config::parse(
            r#"
            font = "Inter"
            hide = "previous"
            key_label = "Hyper"
            warp = "alt+super+f12"

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
        assert_eq!(cfg.by_key('C').unwrap().exec.as_deref(), Some("kitty"));
        assert!(cfg.by_app_id("Kitty").is_some());
    }

    #[test]
    fn defaults() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.hide, Hide::Auto);
        assert_eq!(cfg.key_label(), "Super");
        assert_eq!(cfg.warp(), Some(DEFAULT_WARP));
    }

    #[test]
    fn accepts_the_old_opacity_setting() {
        assert!(Config::parse("opacity = 0.8").is_ok());
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
