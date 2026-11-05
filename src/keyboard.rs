//! The overlay: a keyboard showing what each key does in a layer.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::layout::Layout;
use crate::overlay::{Theme, draw_icon, rounded, text_layout};

/// An app on a key: a pinned letter, or a number for an open, unpinned app.
pub struct KeyApp {
    pub key: char,
    pub name: String,
    pub icon: Option<PathBuf>,
    pub running: bool,
}

// Logical-pixel metrics.
const PAD: f64 = 16.0;
const TITLE_H: f64 = 30.0;
const UNIT: f64 = 50.0;
const GAP: f64 = 4.0;
pub const ICON: f64 = 26.0;

pub struct Keyboard {
    pub layout: Layout,
    pub title: String,
    /// The keys held for the layer, e.g. "Tab" or "Super+;": drawn highlighted.
    pub prefix: String,
    /// Apps, drawn as icons.
    pub apps: Vec<KeyApp>,
    /// Text for other keys, by key label.
    pub labels: HashMap<String, String>,
}

impl Keyboard {
    fn app(&self, label: &str) -> Option<&KeyApp> {
        let mut chars = label.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return None;
        };
        self.apps.iter().find(|r| r.key == c)
    }

    fn is_held(&self, key: &str) -> bool {
        self.prefix.split('+').any(|p| p.eq_ignore_ascii_case(key))
    }

    fn label(&self, key: &str) -> Option<&str> {
        self.labels
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// Icons in the order `paint` draws the keys.
    pub fn icon_paths(&self) -> Vec<Option<PathBuf>> {
        self.layout
            .keys
            .iter()
            .map(|k| self.app(&k.label).and_then(|r| r.icon.clone()))
            .collect()
    }

    pub fn size(&self) -> (f64, f64) {
        (
            PAD * 2.0 + self.layout.width * UNIT - GAP,
            PAD * 2.0 + TITLE_H + self.layout.rows as f64 * UNIT - GAP,
        )
    }
}

pub fn paint(
    cr: &cairo::Context,
    t: &Theme,
    font: &str,
    kb: &Keyboard,
    icons: &[Option<cairo::ImageSurface>],
    scale: f64,
) {
    let layout = |text: &str, px: f64, bold: bool| text_layout(cr, font, text, px, bold);
    let l = layout(&kb.title, 15.0, true);
    l.set_width(((kb.size().0 - PAD * 2.0) * pango::SCALE as f64) as i32);
    l.set_ellipsize(pango::EllipsizeMode::End);
    t.on_surface.set(cr);
    cr.move_to(PAD, PAD + 2.0);
    pangocairo::functions::show_layout(cr, &l);

    let h = UNIT - GAP;
    for (key, icon) in kb.layout.keys.iter().zip(icons) {
        let label = key.label.as_str();
        let x = PAD + key.x * UNIT;
        let y = PAD + TITLE_H + key.row as f64 * UNIT;
        let w = key.width * UNIT - GAP;
        let app = kb.app(label);
        // An app without an icon shows its name instead.
        let bound = kb.label(label).or(match (app, icon) {
            (Some(app), None) => Some(app.name.as_str()),
            _ => None,
        });
        let used = app.is_some() || bound.is_some();
        let held = kb.is_held(label);
        rounded(cr, x, y, w, h, 7.0);
        match (held, used) {
            (true, _) => t.primary.alpha(0.45),
            (_, true) => t.primary.alpha(0.22),
            _ => t.on_surface.alpha(0.07),
        }
        .set(cr);
        cr.fill_preserve().ok();
        t.outline.set(cr);
        cr.set_line_width(1.0);
        cr.stroke().ok();

        let text = if label.len() == 1 {
            label.to_uppercase()
        } else {
            label.to_string()
        };
        let color = if held || used {
            t.on_surface
        } else {
            t.on_surface.alpha(0.65)
        };
        let l = layout(&text, 11.0, held);
        color.set(cr);
        cr.move_to(x + 5.0, y + 3.0);
        pangocairo::functions::show_layout(cr, &l);

        if let Some(bound) = bound {
            let l = layout(bound, 11.5, true);
            l.set_width(((w - 6.0) * pango::SCALE as f64) as i32);
            l.set_ellipsize(pango::EllipsizeMode::End);
            l.set_alignment(pango::Alignment::Center);
            t.primary.set(cr);
            cr.move_to(x + 3.0, y + h - l.pixel_size().1 as f64 - 5.0);
            pangocairo::functions::show_layout(cr, &l);
        }

        if app.is_some_and(|a| a.running) {
            t.primary.set(cr);
            cr.arc(x + w - 7.0, y + 7.0, 3.0, 0.0, std::f64::consts::TAU);
            cr.fill().ok();
        }

        if let (Some(app), Some(img)) = (app, icon) {
            let alpha = if app.running { 1.0 } else { 0.55 };
            draw_icon(
                cr,
                img,
                x + (w - ICON) / 2.0,
                y + h - ICON - 4.0,
                scale,
                alpha,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_every_key_of_the_prefix() {
        let kb = Keyboard {
            layout: Layout::load("ansi").unwrap(),
            title: String::new(),
            prefix: "Super+;".into(),
            apps: Vec::new(),
            labels: HashMap::new(),
        };
        assert!(kb.is_held("Super"));
        assert!(kb.is_held(";"));
        assert!(!kb.is_held("Tab"));
    }
}
