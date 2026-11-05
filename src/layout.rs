//! Keyboard layouts: built in, or from ~/.config/keydock/layouts/<name>.toml.

use serde::Deserialize;

use crate::config::Config;

const BUILT_IN: &[(&str, &str)] = &[
    ("ansi", include_str!("../layouts/ansi.toml")),
    ("corne", include_str!("../layouts/corne.toml")),
];

/// More than any keyboard needs, and small enough to always fit a drawing surface.
const MAX_WIDTH: f64 = 40.0;
const MAX_ROWS: usize = 12;

pub struct Key {
    pub label: String,
    /// Left edge and width in key units.
    pub x: f64,
    pub width: f64,
    pub row: usize,
}

pub struct Layout {
    pub keys: Vec<Key>,
    /// Size in key units.
    pub width: f64,
    pub rows: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    rows: Vec<String>,
}

impl Layout {
    /// The layout for the keyboards connected now, with keys for `numbers` apps.
    pub fn current(cfg: &Config, numbers: usize) -> Result<Layout, String> {
        let mut layout = Self::load(cfg.layout_name(&device_names()))?;
        layout.add_number_row(numbers);
        Ok(layout)
    }

    /// Number keys a layout lacks, as on a 42-key one, go in a row below it, so
    /// apps on the numbers can still be seen and picked.
    fn add_number_row(&mut self, numbers: usize) {
        let missing: Vec<String> = (1..=numbers)
            .map(|n| n.to_string())
            .filter(|n| !self.keys.iter().any(|k| k.label == *n))
            .collect();
        if missing.is_empty() {
            return;
        }
        self.width = self.width.max(missing.len() as f64);
        for (i, label) in missing.into_iter().enumerate() {
            self.keys.push(Key {
                label,
                x: i as f64,
                width: 1.0,
                row: self.rows,
            });
        }
        self.rows += 1;
    }

    /// The user's layout of that name, else the built-in one.
    pub fn load(name: &str) -> Result<Layout, String> {
        let path = Config::path().with_file_name(format!("layouts/{name}.toml"));
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => BUILT_IN
                .iter()
                .find(|(n, _)| *n == name)
                .ok_or(format!("no layout named {name}"))?
                .1
                .to_string(),
        };
        Self::parse(&text).map_err(|e| format!("layout {name}: {e}"))
    }

    fn parse(text: &str) -> Result<Layout, String> {
        let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut keys = Vec::new();
        let mut width: f64 = 0.0;
        for (row, line) in file.rows.iter().enumerate() {
            let mut x = 0.0;
            for token in line.split_whitespace() {
                let (label, units) = split_width(token);
                if label != "_" {
                    keys.push(Key {
                        label: label.to_string(),
                        x,
                        width: units,
                        row,
                    });
                }
                x += units;
            }
            width = width.max(x);
        }
        if keys.is_empty() {
            return Err("no keys".into());
        }
        if width > MAX_WIDTH || file.rows.len() > MAX_ROWS {
            return Err(format!(
                "too large, the limit is {MAX_WIDTH} key units wide and {MAX_ROWS} rows"
            ));
        }
        Ok(Layout {
            keys,
            width,
            rows: file.rows.len(),
        })
    }
}

/// "Tab:1.5" is ("Tab", 1.5); a token without a valid width, such as ":", is one unit wide.
fn split_width(token: &str) -> (&str, f64) {
    token
        .rsplit_once(':')
        .and_then(|(label, width)| Some((label, width.parse().ok()?)))
        .filter(|(label, width)| !label.is_empty() && *width > 0.0)
        .unwrap_or((token, 1.0))
}

/// The names of the connected input devices.
fn device_names() -> Vec<String> {
    std::fs::read_to_string("/proc/bus/input/devices")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("N: Name="))
        .map(|name| name.trim_matches('"').to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_widths_and_gaps() {
        let layout = Layout::parse(r#"rows = ["Tab:1.5 q _:2 :", "a"]"#).unwrap();
        let keys: Vec<_> = layout
            .keys
            .iter()
            .map(|k| (k.label.as_str(), k.x, k.width, k.row))
            .collect();
        assert_eq!(
            keys,
            [
                ("Tab", 0.0, 1.5, 0),
                ("q", 1.5, 1.0, 0),
                (":", 4.5, 1.0, 0),
                ("a", 0.0, 1.0, 1)
            ]
        );
        assert_eq!((layout.width, layout.rows), (5.5, 2));
    }

    #[test]
    fn adds_number_keys_only_where_missing() {
        let mut layout = Layout::load("corne").unwrap();
        let rows = layout.rows;
        layout.add_number_row(0);
        assert_eq!(layout.rows, rows);
        layout.add_number_row(2);
        assert_eq!(layout.rows, rows + 1);
        assert!(layout.keys.iter().any(|k| k.label == "2" && k.row == rows));

        let mut layout = Layout::load("ansi").unwrap();
        let rows = layout.rows;
        layout.add_number_row(2);
        assert_eq!(layout.rows, rows);
    }

    #[test]
    fn adds_only_the_missing_numbers() {
        let mut layout = Layout::parse(r#"rows = ["1 q w"]"#).unwrap();
        layout.add_number_row(2);
        let labels: Vec<_> = layout.keys.iter().map(|k| k.label.as_str()).collect();
        assert_eq!(labels, ["1", "q", "w", "2"]);
        assert_eq!(layout.rows, 2);
    }

    #[test]
    fn rejects_layouts_too_large_to_draw() {
        assert!(Layout::parse(r#"rows = ["q:1000"]"#).is_err());
        assert!(Layout::parse(r#"rows = ["q:inf"]"#).is_err());
        let rows = vec!["\"q\""; 13].join(",");
        assert!(Layout::parse(&format!("rows = [{rows}]")).is_err());
    }

    #[test]
    fn rejects_an_empty_layout() {
        assert!(Layout::parse("rows = []").is_err());
    }

    #[test]
    fn built_in_layouts_parse() {
        for (name, text) in BUILT_IN {
            assert!(Layout::parse(text).is_ok(), "{name}");
        }
    }
}
