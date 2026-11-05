//! The peek view: a keyboard showing what each key does in a layer.

use std::collections::HashMap;

use crate::overlay::{Row, Theme, draw_icon, rounded, text_layout};

/// A laptop ANSI keyboard: key labels and widths in key units.
const ANSI: &[&[(&str, f64)]] = &[
    &[
        ("`", 1.0),
        ("1", 1.0),
        ("2", 1.0),
        ("3", 1.0),
        ("4", 1.0),
        ("5", 1.0),
        ("6", 1.0),
        ("7", 1.0),
        ("8", 1.0),
        ("9", 1.0),
        ("0", 1.0),
        ("-", 1.0),
        ("=", 1.0),
        ("Bksp", 2.0),
    ],
    &[
        ("Tab", 1.5),
        ("q", 1.0),
        ("w", 1.0),
        ("e", 1.0),
        ("r", 1.0),
        ("t", 1.0),
        ("y", 1.0),
        ("u", 1.0),
        ("i", 1.0),
        ("o", 1.0),
        ("p", 1.0),
        ("[", 1.0),
        ("]", 1.0),
        ("\\", 1.5),
    ],
    &[
        ("Caps", 1.75),
        ("a", 1.0),
        ("s", 1.0),
        ("d", 1.0),
        ("f", 1.0),
        ("g", 1.0),
        ("h", 1.0),
        ("j", 1.0),
        ("k", 1.0),
        ("l", 1.0),
        (";", 1.0),
        ("'", 1.0),
        ("Enter", 2.25),
    ],
    &[
        ("Shift", 2.25),
        ("z", 1.0),
        ("x", 1.0),
        ("c", 1.0),
        ("v", 1.0),
        ("b", 1.0),
        ("n", 1.0),
        ("m", 1.0),
        (",", 1.0),
        (".", 1.0),
        ("/", 1.0),
        ("Shift", 2.75),
    ],
    &[
        ("Ctrl", 1.5),
        ("Super", 1.25),
        ("Alt", 1.5),
        ("Space", 7.0),
        ("Alt", 1.5),
        ("Ctrl", 2.25),
    ],
];

// Logical-pixel metrics.
const PAD: f64 = 16.0;
const TITLE_H: f64 = 30.0;
const UNIT: f64 = 50.0;
const GAP: f64 = 4.0;
pub const ICON: f64 = 26.0;

pub struct Keyboard {
    /// The key held for the layer, e.g. "Tab": the title and the highlighted key.
    pub prefix: String,
    /// Pinned apps, drawn as icons; `Row::key` is their letter.
    pub apps: Vec<Row>,
    /// Text for other keys, by key label.
    pub labels: HashMap<String, String>,
}

impl Keyboard {
    fn app(&self, label: &str) -> Option<&Row> {
        let mut chars = label.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return None;
        };
        self.apps.iter().find(|r| r.key == c)
    }

    fn label(&self, key: &str) -> Option<&str> {
        self.labels
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// Icons in the order `paint` draws the keys.
    pub fn icon_paths(&self) -> Vec<Option<std::path::PathBuf>> {
        keys()
            .map(|(label, ..)| self.app(label).and_then(|r| r.icon.clone()))
            .collect()
    }

    pub fn size(&self) -> (f64, f64) {
        let units = ANSI
            .iter()
            .map(|row| row.iter().map(|(_, w)| w).sum::<f64>())
            .fold(0.0, f64::max);
        (
            PAD * 2.0 + units * UNIT - GAP,
            PAD * 2.0 + TITLE_H + ANSI.len() as f64 * UNIT - GAP,
        )
    }
}

/// Every key with its top-left corner and width, in logical pixels.
fn keys() -> impl Iterator<Item = (&'static str, f64, f64, f64)> {
    ANSI.iter().enumerate().flat_map(|(r, row)| {
        let y = PAD + TITLE_H + r as f64 * UNIT;
        let mut x = PAD;
        row.iter().map(move |&(label, units)| {
            let key = (label, x, y, units * UNIT - GAP);
            x += units * UNIT;
            key
        })
    })
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
    let l = layout(&format!("{} + …", kb.prefix), 15.0, true);
    t.on_surface.set(cr);
    cr.move_to(PAD, PAD + 2.0);
    pangocairo::functions::show_layout(cr, &l);

    let h = UNIT - GAP;
    for ((label, x, y, w), icon) in keys().zip(icons) {
        let app = kb.app(label);
        let bound = kb.label(label);
        let used = app.is_some() || bound.is_some();
        let held = label.eq_ignore_ascii_case(&kb.prefix);
        rounded(cr, x, y, w, h, 7.0);
        match (held, used) {
            (true, _) => t.primary.alpha(0.30),
            (_, true) => t.primary.alpha(0.12),
            _ => t.on_surface.alpha(0.04),
        }
        .set(cr);
        cr.fill_preserve().ok();
        t.outline.alpha(0.6).set(cr);
        cr.set_line_width(1.0);
        cr.stroke().ok();

        let text = if label.len() == 1 {
            label.to_uppercase()
        } else {
            label.to_string()
        };
        let color = match (held, used) {
            (true, _) => t.primary,
            (_, true) => t.on_surface,
            _ => t.on_surface_variant,
        };
        let l = layout(&text, 10.5, held);
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
