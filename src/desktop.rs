//! Desktop entries (names, icons, launch ids) and icon-theme lookup.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    /// File name without ".desktop", usable with `gtk-launch`.
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
}

/// Desktop entries indexed by lowercase desktop id and StartupWMClass.
pub struct DesktopIndex {
    by_key: HashMap<String, DesktopEntry>,
}

impl DesktopIndex {
    pub fn load() -> Self {
        Self::from_dirs(&data_dirs(), &locale_keys())
    }

    /// Indexes `<dir>/applications` for each data dir, highest priority first.
    fn from_dirs(dirs: &[PathBuf], locales: &[String]) -> Self {
        let mut by_key = HashMap::new();
        for dir in dirs {
            let Ok(entries) = fs::read_dir(dir.join("applications")) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "desktop") {
                    continue;
                }
                let Some((entry, wm_class)) = parse_entry(&path, locales) else {
                    continue;
                };
                let mut keys = vec![entry.id.to_lowercase()];
                if let Some(class) = wm_class {
                    keys.push(class.to_lowercase());
                }
                for key in keys {
                    // Earlier directories win, so only fill in missing keys.
                    by_key.entry(key).or_insert_with(|| entry.clone());
                }
            }
        }
        DesktopIndex { by_key }
    }

    /// Finds the entry for a Wayland app_id or X11 class, tolerating
    /// versioned ids such as "emacs-32-0-50".
    pub fn lookup(&self, app_id: &str) -> Option<&DesktopEntry> {
        let id = app_id.to_lowercase();
        if let Some(e) = self.by_key.get(&id) {
            return Some(e);
        }
        let mut stem = id.as_str();
        while let Some((head, _)) = stem.rsplit_once('-') {
            if let Some(e) = self.by_key.get(head) {
                return Some(e);
            }
            stem = head;
        }
        // Reverse-DNS ids sometimes ship a desktop file named after the last part.
        id.rsplit('.').next().and_then(|last| self.by_key.get(last))
    }
}

fn parse_entry(path: &Path, locales: &[String]) -> Option<(DesktopEntry, Option<String>)> {
    let text = fs::read_to_string(path).ok()?;
    let mut in_main = false;
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            fields.entry(k.trim()).or_insert(v.trim());
        }
    }
    if fields.get("Type").is_some_and(|t| *t != "Application")
        || fields.get("Hidden") == Some(&"true")
    {
        return None;
    }
    let name = locales
        .iter()
        .find_map(|l| fields.get(format!("Name[{l}]").as_str()))
        .or_else(|| fields.get("Name"))?
        .to_string();
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let icon = fields
        .get("Icon")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let wm_class = fields.get("StartupWMClass").map(|s| s.to_string());
    Some((DesktopEntry { id, name, icon }, wm_class))
}

/// Name[...] keys to try, e.g. ["zh_CN", "zh"] for a Chinese session.
fn locale_keys() -> Vec<String> {
    let mut keys = Vec::new();
    for lang in crate::i18n::languages() {
        let base = lang.split('_').next().unwrap_or("").to_string();
        for key in [lang, base] {
            if !key.is_empty() && !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// XDG data directories, highest priority first.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share")),
    ];
    let system =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(
        system
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
    );
    dirs
}

/// Resolves icon names to files using the GTK icon theme and its parents.
pub struct IconFinder {
    roots: Vec<PathBuf>,
    themes: Vec<String>,
    cache: HashMap<String, Option<PathBuf>>,
}

impl IconFinder {
    pub fn new() -> Self {
        let mut roots = vec![home().join(".icons")];
        roots.extend(data_dirs().into_iter().map(|d| d.join("icons")));
        Self::with(roots, &gtk_icon_theme().unwrap_or_else(|| "hicolor".into()))
    }

    /// Uses `theme`, the themes it inherits, and hicolor, found under `roots`.
    fn with(roots: Vec<PathBuf>, theme: &str) -> Self {
        let mut finder = IconFinder {
            roots,
            themes: Vec::new(),
            cache: HashMap::new(),
        };
        let mut pending = vec![theme.to_string()];
        while let Some(theme) = pending.pop() {
            if finder.themes.contains(&theme) {
                continue;
            }
            let parents = finder.theme_parents(&theme);
            finder.themes.push(theme);
            pending.extend(parents.into_iter().rev());
        }
        if !finder.themes.iter().any(|t| t == "hicolor") {
            finder.themes.push("hicolor".into());
        }
        finder
    }

    fn theme_parents(&self, theme: &str) -> Vec<String> {
        for root in &self.roots {
            if let Ok(text) = fs::read_to_string(root.join(theme).join("index.theme")) {
                for line in text.lines() {
                    if let Some(v) = line.strip_prefix("Inherits=") {
                        return v
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                }
                return Vec::new();
            }
        }
        Vec::new()
    }

    pub fn find(&mut self, name: &str) -> Option<PathBuf> {
        if let Some(hit) = self.cache.get(name) {
            return hit.clone();
        }
        let found = self.search(name);
        self.cache.insert(name.to_string(), found.clone());
        found
    }

    fn search(&self, name: &str) -> Option<PathBuf> {
        let direct = Path::new(name);
        if direct.is_absolute() {
            return direct.exists().then(|| direct.to_path_buf());
        }
        for theme in &self.themes {
            for root in &self.roots {
                for dir in icon_dirs(&root.join(theme)) {
                    for ext in ["svg", "png"] {
                        let p = dir.join(format!("{name}.{ext}"));
                        if p.exists() {
                            return Some(p);
                        }
                    }
                }
            }
        }
        for ext in ["svg", "png"] {
            let p = PathBuf::from(format!("/usr/share/pixmaps/{name}.{ext}"));
            if p.exists() {
                return Some(p);
            }
        }
        None
    }
}

/// App icon directories of a theme, best first: scalable, then the smallest
/// size of at least 48px, then larger, then smaller ones.
fn icon_dirs(base: &Path) -> Vec<PathBuf> {
    let rank = |size: &str| -> Option<u32> {
        if size == "scalable" {
            return Some(0);
        }
        let n: u32 = size.split(['x', '@']).next()?.parse().ok()?;
        Some(if n >= 48 { n } else { 10_000 - n })
    };
    let mut dirs: Vec<(u32, PathBuf)> = Vec::new();
    // Both "48x48/apps" (Papirus, hicolor) and "apps/48" (Breeze) layouts exist.
    for (parent, child_is_size) in [(base.to_path_buf(), true), (base.join("apps"), false)] {
        let Ok(entries) = fs::read_dir(&parent) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(r) = rank(&name) else { continue };
            let dir = if child_is_size {
                e.path().join("apps")
            } else {
                e.path()
            };
            if dir.is_dir() {
                dirs.push((r, dir));
            }
        }
    }
    dirs.sort();
    dirs.into_iter().map(|(_, d)| d).collect()
}

fn gtk_icon_theme() -> Option<String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    for file in ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"] {
        if let Ok(text) = fs::read_to_string(base.join(file)) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=')
                    && k.trim() == "gtk-icon-theme-name"
                {
                    return Some(v.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keydock-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: PathBuf, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_entries_by_id_class_and_versioned_id() {
        let (user, system) = (temp_dir("apps-user"), temp_dir("apps-system"));
        write(
            system.join("applications/qq.desktop"),
            "[Desktop Entry]\nType=Application\nName=QQ\nName[zh_CN]=腾讯QQ\nIcon=qq\nStartupWMClass=QQ\n",
        );
        write(
            system.join("applications/emacs.desktop"),
            "[Desktop Entry]\nType=Application\nName=Emacs\n",
        );
        write(
            user.join("applications/emacs.desktop"),
            "[Desktop Entry]\nType=Application\nName=My Emacs\n",
        );
        write(
            system.join("applications/hidden.desktop"),
            "[Desktop Entry]\nName=Gone\nHidden=true\n",
        );
        let index = DesktopIndex::from_dirs(
            &[user.clone(), system.clone()],
            &["zh_CN".into(), "zh".into()],
        );
        fs::remove_dir_all(user).unwrap();
        fs::remove_dir_all(system).unwrap();

        let qq = index.lookup("QQ").unwrap();
        assert_eq!(
            (qq.id.as_str(), qq.name.as_str(), qq.icon.as_deref()),
            ("qq", "腾讯QQ", Some("qq"))
        );
        assert_eq!(index.lookup("emacs-32-0-50").unwrap().name, "My Emacs");
        assert!(index.lookup("hidden").is_none());
    }

    #[test]
    fn prefers_scalable_then_large_icons_and_follows_inheritance() {
        let root = temp_dir("icons");
        write(
            root.join("Child/index.theme"),
            "[Icon Theme]\nInherits=Parent\n",
        );
        write(root.join("Child/16x16/apps/term.png"), "");
        write(root.join("Child/256x256/apps/term.png"), "");
        write(root.join("Child/64x64/apps/term.png"), "");
        write(root.join("Parent/scalable/apps/web.svg"), "");
        write(root.join("hicolor/512x512/apps/chat.png"), "");
        let mut finder = IconFinder::with(vec![root.clone()], "Child");
        let found = |f: &mut IconFinder, name| {
            f.find(name)
                .map(|p| p.strip_prefix(&root).unwrap().to_path_buf())
        };

        assert_eq!(
            found(&mut finder, "term"),
            Some("Child/64x64/apps/term.png".into())
        );
        assert_eq!(
            found(&mut finder, "web"),
            Some("Parent/scalable/apps/web.svg".into())
        );
        assert_eq!(
            found(&mut finder, "chat"),
            Some("hicolor/512x512/apps/chat.png".into())
        );
        assert_eq!(found(&mut finder, "missing"), None);
        fs::remove_dir_all(root).unwrap();
    }
}
